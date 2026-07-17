//! Random-access elevation sampling over the windowed DEM reader.
//!
//! Bilinear interpolation needs the four pixel-center neighbours of an
//! arbitrary geographic point, which routinely straddle internal chunk
//! boundaries. A small LRU of decoded chunks keeps that random access
//! memory-bounded: at the default capacity of 16 the cache tops out at
//! 16 × 256×256 × 4B = 4MB of f32 — the reader's one-chunk posture scales to
//! a working set, never to the raster.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read, Seek};
use std::path::Path;

use crate::reader::{DemError, DemWindow, GeoTransform, WindowedDemReader};

/// Default chunk-cache capacity (≈4MB of decoded f32 at 256×256 chunks).
pub const DEFAULT_CACHE_CHUNKS: usize = 16;

/// Bilinear elevation sampler with a bounded decoded-chunk cache.
pub struct DemSampler<R: Read + Seek> {
    reader: WindowedDemReader<R>,
    transform: GeoTransform,
    cache: HashMap<(u32, u32), DemWindow>,
    lru: Vec<(u32, u32)>,
    capacity: usize,
}

impl DemSampler<BufReader<File>> {
    pub fn open(path: &Path) -> Result<Self, DemError> {
        Self::new(WindowedDemReader::open(path)?)
    }
}

impl<R: Read + Seek> DemSampler<R> {
    /// Wraps a reader; fails fast if the TIFF carries no georeferencing.
    pub fn new(reader: WindowedDemReader<R>) -> Result<Self, DemError> {
        Self::with_cache_capacity(reader, DEFAULT_CACHE_CHUNKS)
    }

    pub fn with_cache_capacity(
        reader: WindowedDemReader<R>,
        capacity: usize,
    ) -> Result<Self, DemError> {
        let transform = reader
            .geo_transform()
            .ok_or(DemError::MissingGeoTransform)?;
        Ok(Self {
            reader,
            transform,
            cache: HashMap::new(),
            lru: Vec::new(),
            capacity: capacity.max(1),
        })
    }

    pub fn transform(&self) -> GeoTransform {
        self.transform
    }

    pub fn reader(&self) -> &WindowedDemReader<R> {
        &self.reader
    }

    /// Bilinear elevation at a model-space point (lon/lat degrees on the
    /// fixture). NoData and beyond-the-raster neighbours drop out of the
    /// weighting; a point with no finite neighbour at all is NaN.
    pub fn elevation_at_geo(&mut self, x: f64, y: f64) -> Result<f32, DemError> {
        let (px, py) = self.transform.model_to_pixel(x, y);
        let (x0, y0) = (px.floor(), py.floor());
        let (fx, fy) = (px - x0, py - y0);
        let (x0, y0) = (x0 as i64, y0 as i64);

        let mut weight_sum = 0.0f64;
        let mut value_sum = 0.0f64;
        for (dx, dy, w) in [
            (0, 0, (1.0 - fx) * (1.0 - fy)),
            (1, 0, fx * (1.0 - fy)),
            (0, 1, (1.0 - fx) * fy),
            (1, 1, fx * fy),
        ] {
            if w == 0.0 {
                continue;
            }
            let v = self.pixel(x0 + dx, y0 + dy)?;
            if v.is_finite() {
                weight_sum += w;
                value_sum += f64::from(v) * w;
            }
        }
        Ok(if weight_sum > 0.0 {
            (value_sum / weight_sum) as f32
        } else {
            f32::NAN
        })
    }

    /// Raw pixel-center lookup; positions outside the raster are NaN.
    fn pixel(&mut self, ix: i64, iy: i64) -> Result<f32, DemError> {
        let (w, h) = self.reader.dimensions();
        if ix < 0 || iy < 0 || ix >= i64::from(w) || iy >= i64::from(h) {
            return Ok(f32::NAN);
        }
        let (cw, ch) = self.reader.chunk_dimensions();
        let (col, row) = (ix as u32 / cw, iy as u32 / ch);
        let (lx, ly) = ((ix as u32 % cw) as usize, (iy as u32 % ch) as usize);
        let window = self.window(col, row)?;
        Ok(window.elevations[ly * window.width + lx])
    }

    /// Cache-through chunk access with LRU eviction at `capacity`.
    fn window(&mut self, col: u32, row: u32) -> Result<&DemWindow, DemError> {
        let key = (col, row);
        if !self.cache.contains_key(&key) {
            let window = self.reader.read_window(col, row)?;
            if self.cache.len() >= self.capacity {
                let evicted = self.lru.remove(0);
                self.cache.remove(&evicted);
            }
            self.cache.insert(key, window);
        }
        if self.lru.last() != Some(&key) {
            self.lru.retain(|k| *k != key);
            self.lru.push(key);
        }
        Ok(&self.cache[&key])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader::test_dem;

    const ORIGIN: (f64, f64) = (11.0, 47.5);
    const SCALE: f64 = 0.01;

    /// 20×10 raster, elevation = 500 + 10x + 5y, NoData at pixel (5,5).
    fn sampler() -> DemSampler<std::io::Cursor<Vec<u8>>> {
        let dem = test_dem::build(20, 10, 9999, Some((ORIGIN.0, ORIGIN.1, SCALE)), |x, y| {
            if (x, y) == (5, 5) {
                9999
            } else {
                (500 + 10 * x + 5 * y) as u16
            }
        });
        DemSampler::new(WindowedDemReader::new(dem).unwrap()).unwrap()
    }

    /// Model coords of pixel center (x,y).
    fn center(x: f64, y: f64) -> (f64, f64) {
        (ORIGIN.0 + (x + 0.5) * SCALE, ORIGIN.1 - (y + 0.5) * SCALE)
    }

    #[test]
    fn pixel_centers_sample_exactly() {
        let mut s = sampler();
        let (lon, lat) = center(3.0, 2.0);
        assert_eq!(s.elevation_at_geo(lon, lat).unwrap(), 540.0);
    }

    #[test]
    fn midpoints_interpolate_bilinearly() {
        let mut s = sampler();
        // Halfway between (3,2)=540 and (4,2)=550.
        let (lon, lat) = center(3.5, 2.0);
        assert!((s.elevation_at_geo(lon, lat).unwrap() - 545.0).abs() < 1e-3);
        // Center of the (3,2)(4,2)(3,3)(4,3) quad: mean of 540/550/545/555.
        let (lon, lat) = center(3.5, 2.5);
        assert!((s.elevation_at_geo(lon, lat).unwrap() - 547.5).abs() < 1e-3);
    }

    #[test]
    fn nodata_neighbours_drop_out_of_the_weighting() {
        let mut s = sampler();
        // Halfway between NoData (5,5) and finite (6,5)=585: the finite
        // neighbour carries all the weight.
        let (lon, lat) = center(5.5, 5.0);
        assert_eq!(s.elevation_at_geo(lon, lat).unwrap(), 585.0);
    }

    #[test]
    fn outside_the_raster_is_nan() {
        let mut s = sampler();
        assert!(s.elevation_at_geo(10.0, 47.5).unwrap().is_nan());
        assert!(s.elevation_at_geo(11.05, 48.5).unwrap().is_nan());
    }

    #[test]
    fn ungeoreferenced_dem_is_rejected() {
        let dem = test_dem::build(4, 4, 9999, None, |_, _| 500);
        assert!(matches!(
            DemSampler::new(WindowedDemReader::new(dem).unwrap()),
            Err(DemError::MissingGeoTransform)
        ));
    }
}
