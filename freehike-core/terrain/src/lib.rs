//! `terrain` — DEM GeoTIFF processing (Phase 6).
//!
//! P6.C1 pipeline: windowed GeoTIFF reads (one internal chunk at a time,
//! never the whole raster) → Terrain-RGB transform (Mapbox encoding, base
//! −10000 / interval 0.1) → lossless WebP tiles. Later chunks add the
//! z5–12 tile pyramid, `terrain.pmtiles` assembly through the Phase-5
//! writer, and the Surface-v1 budget-yield cursor.

pub mod reader;
pub mod rgb;
pub mod webp;

use std::path::Path;

use reader::{DemError, WindowedDemReader};

/// Crate identity used by walking-skeleton diagnostics.
pub const CRATE: &str = "terrain";

/// Errors from the end-to-end window→WebP path.
#[derive(Debug)]
pub enum TerrainError {
    Dem(DemError),
    Webp(image::ImageError),
}

impl std::fmt::Display for TerrainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TerrainError::Dem(e) => write!(f, "terrain dem: {e}"),
            TerrainError::Webp(e) => write!(f, "terrain webp: {e}"),
        }
    }
}

impl std::error::Error for TerrainError {}

impl From<DemError> for TerrainError {
    fn from(e: DemError) -> Self {
        TerrainError::Dem(e)
    }
}

impl From<image::ImageError> for TerrainError {
    fn from(e: image::ImageError) -> Self {
        TerrainError::Webp(e)
    }
}

/// End-to-end convenience: decode DEM window (`col`, `row`) and return it as
/// a 256×256 lossless Terrain-RGB WebP tile. Peak memory is one chunk plus
/// the 192KB RGB buffer.
pub fn dem_window_to_webp_tile(
    dem_path: &Path,
    col: u32,
    row: u32,
) -> Result<Vec<u8>, TerrainError> {
    let mut reader = WindowedDemReader::open(dem_path)?;
    let window = reader.read_window(col, row)?;
    let rgb_buf = rgb::window_to_terrain_rgb(&window)?;
    Ok(webp::encode_rgb_lossless(
        &rgb_buf,
        rgb::TILE_SIZE as u32,
        rgb::TILE_SIZE as u32,
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// L2 real-data proof over the Phase-6 fixture. Ignored so the L1 ladder
    /// stays fixture-independent; run:
    ///   cargo test -p terrain --release -- --ignored --nocapture real_innsbruck
    #[test]
    #[ignore]
    fn real_innsbruck_dem_window_to_webp() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../offline_sandbox/raw_data/innsbruck_dem.tif");

        let mut reader = WindowedDemReader::open(&fixture).unwrap();
        assert_eq!(reader.dimensions(), (1800, 1260));
        assert_eq!(reader.chunk_dimensions(), (256, 256));
        assert_eq!(reader.window_grid(), (8, 5));
        assert_eq!(reader.nodata(), Some(-32768.0));

        // Interior window over the Inn valley / Nordkette flank: fully
        // populated, plausibly alpine.
        let window = reader.read_window(1, 1).unwrap();
        assert_eq!((window.width, window.height), (256, 256));
        let (lo, hi) = window.elevation_range().unwrap();
        assert!(
            lo > 400.0 && hi < 4000.0 && hi > lo + 100.0,
            "implausible alpine relief: {lo}..{hi}"
        );

        // Full pipeline, then decode the WebP and prove losslessness by
        // recovering the source elevations to the 0.1m encoding step.
        let tile = dem_window_to_webp_tile(&fixture, 1, 1).unwrap();
        let decoded = image::load_from_memory(&tile).unwrap().into_rgb8();
        assert_eq!(decoded.dimensions(), (256, 256));
        for (i, px) in decoded.pixels().enumerate() {
            let back = rgb::rgb_to_elevation(px.0);
            let src = f64::from(window.elevations[i]);
            assert!(
                (back - src).abs() <= 0.05 + 1e-6,
                "pixel {i}: {src}m decoded as {back}m"
            );
        }
    }
}
