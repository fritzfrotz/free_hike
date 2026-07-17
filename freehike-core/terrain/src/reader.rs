//! Windowed GeoTIFF DEM reader.
//!
//! Decodes one internal TIFF chunk at a time via `Decoder::read_chunk`, which
//! seeks to that chunk's byte range (TileOffsets/TileByteCounts) and inflates
//! only it. The full raster is never resident: peak heap per window on the
//! Innsbruck fixture is one 256×256 chunk — 128KB of i16 samples plus the
//! 256KB f32 conversion — far under the 50MB ceiling regardless of raster
//! extent.
//!
//! Works on both tiled and striped layouts (a strip is just a full-width
//! chunk); memory stays O(one chunk) either way.

use std::fmt;
use std::fs::File;
use std::io::{BufReader, Read, Seek};
use std::path::Path;

use tiff::decoder::{Decoder, DecodingResult};
use tiff::tags::Tag;
use tiff::TiffError;

/// Errors from windowed DEM access.
#[derive(Debug)]
pub enum DemError {
    Io(std::io::Error),
    Tiff(TiffError),
    /// Sample format the Terrain-RGB path does not handle (e.g. palette).
    UnsupportedSampleFormat(&'static str),
    WindowOutOfRange {
        col: u32,
        row: u32,
        cols: u32,
        rows: u32,
    },
    /// A decoded window exceeds the 256×256 output tile (oversized strips).
    WindowLargerThanTile {
        width: usize,
        height: usize,
    },
    /// An elevation grid handed to the RGB transform is not 256×256.
    GridSizeMismatch {
        got: usize,
    },
    /// The TIFF carries no usable ModelTiepoint + ModelPixelScale pair, so
    /// geographic sampling is impossible.
    MissingGeoTransform,
}

impl fmt::Display for DemError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DemError::Io(e) => write!(f, "dem io: {e}"),
            DemError::Tiff(e) => write!(f, "dem tiff: {e}"),
            DemError::UnsupportedSampleFormat(t) => {
                write!(f, "unsupported DEM sample format: {t}")
            }
            DemError::WindowOutOfRange {
                col,
                row,
                cols,
                rows,
            } => write!(f, "window ({col},{row}) outside {cols}×{rows} chunk grid"),
            DemError::WindowLargerThanTile { width, height } => write!(
                f,
                "decoded window {width}×{height} exceeds the 256×256 tile"
            ),
            DemError::GridSizeMismatch { got } => {
                write!(f, "elevation grid has {got} samples, expected 256×256")
            }
            DemError::MissingGeoTransform => {
                write!(
                    f,
                    "DEM carries no ModelTiepoint/ModelPixelScale georeferencing"
                )
            }
        }
    }
}

/// Affine georeferencing of a north-up raster, parsed from the GeoTIFF
/// ModelTiepointTag + ModelPixelScaleTag pair. Model space is whatever CRS
/// the producer used — geographic degrees (EPSG:4326) on the Innsbruck
/// fixture.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GeoTransform {
    /// Model X/Y of the raster's outer top-left corner.
    pub origin_x: f64,
    pub origin_y: f64,
    /// Model units per pixel; both positive, Y decreasing southward
    /// (north-up raster).
    pub scale_x: f64,
    pub scale_y: f64,
}

impl GeoTransform {
    /// Model coordinates → continuous sample coordinates in which INTEGER
    /// values are pixel centers. The fixture is PixelIsArea (GeoKey 1025 = 1,
    /// the GDAL default): the tiepoint pins the outer corner of pixel (0,0),
    /// so its center sits half a pixel inside — hence the −0.5.
    pub fn model_to_pixel(&self, x: f64, y: f64) -> (f64, f64) {
        (
            (x - self.origin_x) / self.scale_x - 0.5,
            (self.origin_y - y) / self.scale_y - 0.5,
        )
    }

    /// Inverse of [`Self::model_to_pixel`]: pixel-center coordinates → model.
    pub fn pixel_to_model(&self, px: f64, py: f64) -> (f64, f64) {
        (
            self.origin_x + (px + 0.5) * self.scale_x,
            self.origin_y - (py + 0.5) * self.scale_y,
        )
    }
}

impl std::error::Error for DemError {}

impl From<std::io::Error> for DemError {
    fn from(e: std::io::Error) -> Self {
        DemError::Io(e)
    }
}

impl From<TiffError> for DemError {
    fn from(e: TiffError) -> Self {
        DemError::Tiff(e)
    }
}

/// One decoded DEM window: the samples of a single internal TIFF chunk.
///
/// Edge chunks are clipped to the raster extent, so `width`/`height` can be
/// smaller than the nominal chunk size. NoData samples are resolved to NaN.
pub struct DemWindow {
    pub col: u32,
    pub row: u32,
    pub width: usize,
    pub height: usize,
    /// Row-major `width × height` elevations in metres; NoData → `f32::NAN`.
    pub elevations: Vec<f32>,
}

impl DemWindow {
    /// Min/max over finite samples; `None` if the window is all NoData.
    pub fn elevation_range(&self) -> Option<(f32, f32)> {
        self.elevations
            .iter()
            .copied()
            .filter(|e| e.is_finite())
            .fold(None, |acc, e| match acc {
                None => Some((e, e)),
                Some((lo, hi)) => Some((lo.min(e), hi.max(e))),
            })
    }
}

/// Windowed reader over a single-band DEM GeoTIFF.
pub struct WindowedDemReader<R: Read + Seek> {
    decoder: Decoder<R>,
    width: u32,
    height: u32,
    chunk_width: u32,
    chunk_height: u32,
    cols: u32,
    rows: u32,
    nodata: Option<f64>,
    geo_transform: Option<GeoTransform>,
}

impl WindowedDemReader<BufReader<File>> {
    /// Opens a DEM from disk. Only the header/IFD is parsed here; raster
    /// bytes are read chunk-by-chunk in [`Self::read_window`].
    pub fn open(path: &Path) -> Result<Self, DemError> {
        Self::new(BufReader::new(File::open(path)?))
    }
}

impl<R: Read + Seek> WindowedDemReader<R> {
    pub fn new(reader: R) -> Result<Self, DemError> {
        let mut decoder = Decoder::new(reader)?;
        let (width, height) = decoder.dimensions()?;
        let (chunk_width, chunk_height) = decoder.chunk_dimensions();
        // GDAL writes NoData as the ASCII tag 42113.
        let nodata = match decoder.find_tag(Tag::GdalNodata)? {
            Some(v) => v.into_string()?.trim().parse::<f64>().ok(),
            None => None,
        };
        let geo_transform = Self::parse_geo_transform(&mut decoder)?;
        Ok(Self {
            decoder,
            width,
            height,
            chunk_width,
            chunk_height,
            cols: width.div_ceil(chunk_width),
            rows: height.div_ceil(chunk_height),
            nodata,
            geo_transform,
        })
    }

    /// Reads the ModelTiepoint + ModelPixelScale pair into an affine
    /// transform. A tiepoint pins raster position (i,j) to model (X,Y); GDAL
    /// writes (0,0) but the general form is handled. ModelTransformationTag
    /// (full 4×4 matrices, rotated rasters) is out of scope for DEM inputs.
    fn parse_geo_transform(decoder: &mut Decoder<R>) -> Result<Option<GeoTransform>, DemError> {
        let scale = decoder
            .find_tag(Tag::ModelPixelScaleTag)?
            .map(|v| v.into_f64_vec())
            .transpose()?;
        let tie = decoder
            .find_tag(Tag::ModelTiepointTag)?
            .map(|v| v.into_f64_vec())
            .transpose()?;
        Ok(match (scale, tie) {
            (Some(s), Some(t)) if s.len() >= 2 && t.len() >= 6 && s[0] > 0.0 && s[1] > 0.0 => {
                Some(GeoTransform {
                    origin_x: t[3] - t[0] * s[0],
                    origin_y: t[4] + t[1] * s[1],
                    scale_x: s[0],
                    scale_y: s[1],
                })
            }
            _ => None,
        })
    }

    /// Raster extent in pixels.
    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Nominal internal chunk size (tile size, or full-width strip).
    pub fn chunk_dimensions(&self) -> (u32, u32) {
        (self.chunk_width, self.chunk_height)
    }

    /// Window grid: chunks across × chunks down.
    pub fn window_grid(&self) -> (u32, u32) {
        (self.cols, self.rows)
    }

    /// NoData sentinel declared by the producer, if any.
    pub fn nodata(&self) -> Option<f64> {
        self.nodata
    }

    /// Affine georeferencing, if the TIFF carries one.
    pub fn geo_transform(&self) -> Option<GeoTransform> {
        self.geo_transform
    }

    /// Full raster extent in model space (west, south, east, north): the
    /// outer edges of the corner pixels (PixelIsArea), not their centers.
    pub fn geo_bounds(&self) -> Option<(f64, f64, f64, f64)> {
        self.geo_transform.map(|gt| {
            (
                gt.origin_x,
                gt.origin_y - f64::from(self.height) * gt.scale_y,
                gt.origin_x + f64::from(self.width) * gt.scale_x,
                gt.origin_y,
            )
        })
    }

    /// Decodes the single chunk at grid position (`col`, `row`).
    pub fn read_window(&mut self, col: u32, row: u32) -> Result<DemWindow, DemError> {
        if col >= self.cols || row >= self.rows {
            return Err(DemError::WindowOutOfRange {
                col,
                row,
                cols: self.cols,
                rows: self.rows,
            });
        }
        // TIFF chunks are laid out row-major across the grid.
        let index = row * self.cols + col;
        let (w, h) = self.decoder.chunk_data_dimensions(index);
        let elevations = match self.decoder.read_chunk(index)? {
            DecodingResult::I16(v) => self.to_f32(v),
            DecodingResult::I32(v) => self.to_f32(v),
            DecodingResult::U16(v) => self.to_f32(v),
            DecodingResult::U8(v) => self.to_f32(v),
            DecodingResult::F32(v) => self.to_f32(v),
            DecodingResult::F64(v) => self.to_f32(v),
            _ => return Err(DemError::UnsupportedSampleFormat("non-numeric chunk")),
        };
        Ok(DemWindow {
            col,
            row,
            width: w as usize,
            height: h as usize,
            elevations,
        })
    }

    /// Converts raw samples to f32 metres, resolving NoData to NaN. The
    /// comparison runs in f64 so integer sentinels like -32768 match exactly.
    fn to_f32<T: Copy + Into<f64>>(&self, samples: Vec<T>) -> Vec<f32> {
        samples
            .into_iter()
            .map(|s| {
                let v: f64 = s.into();
                match self.nodata {
                    Some(nd) if v == nd => f32::NAN,
                    _ => v as f32,
                }
            })
            .collect()
    }
}

/// In-memory synthetic GeoTIFF builder shared by the L1 tests across this
/// crate's modules (L1 stays fixture-independent; the real DEM is L2-only).
#[cfg(test)]
pub(crate) mod test_dem {
    use std::io::Cursor;
    use tiff::encoder::{colortype, TiffEncoder};
    use tiff::tags::Tag;

    /// Single-strip Gray16 TIFF: `sample(x, y)` supplies each raw value,
    /// `nodata` becomes the GDAL NoData tag, and `geo` (origin_x, origin_y,
    /// scale) is written as the ModelTiepoint/ModelPixelScale pair when set.
    pub(crate) fn build(
        width: u32,
        height: u32,
        nodata: u16,
        geo: Option<(f64, f64, f64)>,
        sample: impl Fn(u32, u32) -> u16,
    ) -> Cursor<Vec<u8>> {
        let mut buf = Cursor::new(Vec::new());
        {
            let mut enc = TiffEncoder::new(&mut buf).unwrap();
            let mut img = enc.new_image::<colortype::Gray16>(width, height).unwrap();
            img.encoder()
                .write_tag(Tag::GdalNodata, nodata.to_string().as_str())
                .unwrap();
            if let Some((ox, oy, scale)) = geo {
                img.encoder()
                    .write_tag(Tag::ModelPixelScaleTag, &[scale, scale, 0.0][..])
                    .unwrap();
                img.encoder()
                    .write_tag(Tag::ModelTiepointTag, &[0.0, 0.0, 0.0, ox, oy, 0.0][..])
                    .unwrap();
            }
            let data: Vec<u16> = (0..height)
                .flat_map(|y| (0..width).map(move |x| (x, y)))
                .map(|(x, y)| sample(x, y))
                .collect();
            img.write_data(&data).unwrap();
        }
        buf.set_position(0);
        buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// NoData at sample 0, gentle gradient elsewhere, no georeferencing.
    fn synthetic_dem(width: u32, height: u32, nodata: u16) -> Cursor<Vec<u8>> {
        test_dem::build(width, height, nodata, None, move |x, y| {
            if (x, y) == (0, 0) {
                nodata
            } else {
                500 + ((y * width + x) % 100) as u16
            }
        })
    }

    #[test]
    fn windowed_read_resolves_nodata_and_grid() {
        let mut reader = WindowedDemReader::new(synthetic_dem(16, 12, 9999)).unwrap();
        assert_eq!(reader.dimensions(), (16, 12));
        assert_eq!(reader.nodata(), Some(9999.0));

        let (cols, rows) = reader.window_grid();
        let win = reader.read_window(0, 0).unwrap();
        assert_eq!((win.col, win.row), (0, 0));
        assert_eq!(win.elevations.len(), win.width * win.height);
        // Sample 0 carried the NoData sentinel.
        assert!(win.elevations[0].is_nan());
        assert_eq!(win.elevations[1], 501.0);

        // Out-of-range window is rejected, not decoded.
        assert!(matches!(
            reader.read_window(cols, rows),
            Err(DemError::WindowOutOfRange { .. })
        ));
    }

    #[test]
    fn elevation_range_ignores_nodata() {
        let mut reader = WindowedDemReader::new(synthetic_dem(8, 4, 9999)).unwrap();
        let win = reader.read_window(0, 0).unwrap();
        let (lo, hi) = win.elevation_range().unwrap();
        assert!(lo >= 500.0 && hi < 600.0, "range {lo}..{hi}");
    }

    #[test]
    fn geo_transform_parses_and_inverts() {
        // Innsbruck-fixture-shaped georeferencing on a small raster.
        let dem = test_dem::build(
            8,
            4,
            9999,
            Some((11.099861, 47.450139, 0.000278)),
            |_, _| 500,
        );
        let reader = WindowedDemReader::new(dem).unwrap();
        let gt = reader.geo_transform().expect("geo transform present");
        assert_eq!(gt.origin_x, 11.099861);
        assert_eq!(gt.origin_y, 47.450139);
        assert_eq!(gt.scale_x, 0.000278);

        // Pixel (0,0) center sits half a pixel inside the origin corner
        // (PixelIsArea), and the mapping round-trips.
        let (px, py) = gt.model_to_pixel(11.099861 + 0.5 * 0.000278, 47.450139 - 0.5 * 0.000278);
        assert!(px.abs() < 1e-9 && py.abs() < 1e-9, "center at ({px},{py})");
        let (x, y) = gt.pixel_to_model(3.0, 2.0);
        let (px, py) = gt.model_to_pixel(x, y);
        assert!((px - 3.0).abs() < 1e-9 && (py - 2.0).abs() < 1e-9);
    }

    #[test]
    fn missing_geo_tags_yield_none() {
        let reader = WindowedDemReader::new(synthetic_dem(8, 4, 9999)).unwrap();
        assert!(reader.geo_transform().is_none());
    }
}
