// SPDX-License-Identifier: Apache-2.0
//! `terrain` — DEM GeoTIFF processing (Phase 6).
//!
//! Pipeline (P6.C1 + P6.C2): windowed GeoTIFF reads (one internal chunk at a
//! time, never the whole raster) → WebMercator (z,x,y) reprojection with
//! chunk-cached bilinear resampling → Terrain-RGB transform (Mapbox
//! encoding, base −10000 / interval 0.1) → lossless WebP tiles. Later
//! chunks add `terrain.pmtiles` assembly through the Phase-5 writer and the
//! Surface-v1 budget-yield cursor.

pub mod archive;
pub mod mercator;
pub mod pyramid;
pub mod reader;
pub mod rgb;
pub mod sample;
pub mod webp;

use std::path::Path;

use reader::{DemError, WindowedDemReader};

/// Crate identity used by walking-skeleton diagnostics.
pub const CRATE: &str = "terrain";

/// Errors from the end-to-end window→WebP path and archive assembly.
#[derive(Debug)]
pub enum TerrainError {
    Dem(DemError),
    Webp(image::ImageError),
    /// Archive-side file I/O (temp data file, final `.pmtiles`).
    Io(std::io::Error),
    /// Non-resumable-as-is state: torn/foreign checkpoint, or a data temp
    /// file that disagrees with it. Never silently restarted (house rule).
    Corrupt(String),
}

impl std::fmt::Display for TerrainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TerrainError::Dem(e) => write!(f, "terrain dem: {e}"),
            TerrainError::Webp(e) => write!(f, "terrain webp: {e}"),
            TerrainError::Io(e) => write!(f, "terrain archive io: {e}"),
            TerrainError::Corrupt(what) => write!(f, "terrain corrupt state: {what}"),
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

impl From<std::io::Error> for TerrainError {
    fn from(e: std::io::Error) -> Self {
        TerrainError::Io(e)
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

    /// L2 real-data proof for the P6.C2 pyramid path: render the known z12
    /// Innsbruck tile from the real DEM and cross-check decoded elevations
    /// against direct sampler queries. Run:
    ///   cargo test -p terrain --release -- --ignored --nocapture real_innsbruck
    #[test]
    #[ignore]
    fn real_innsbruck_z12_tile_renders_plausibly() {
        use mercator::{pixel_center_lat, pixel_center_lon, TileCoord};
        use sample::DemSampler;

        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../offline_sandbox/raw_data/innsbruck_dem.tif");
        let coord = TileCoord {
            z: 12,
            x: 2177,
            y: 1436,
        };

        let tile = pyramid::dem_tile_to_webp(&fixture, coord).unwrap();
        let decoded = image::load_from_memory(&tile.webp).unwrap().into_rgb8();
        assert_eq!(decoded.dimensions(), (256, 256));

        // The tile sits fully inside the DEM: every pixel must be a real
        // alpine elevation, no 0m NoData fill anywhere.
        let elevations: Vec<f64> = decoded
            .pixels()
            .map(|p| rgb::rgb_to_elevation(p.0))
            .collect();
        let lo = elevations.iter().cloned().fold(f64::INFINITY, f64::min);
        let hi = elevations.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        assert!(
            lo > 400.0 && hi < 4000.0 && hi > lo + 100.0,
            "implausible alpine relief: {lo}..{hi}"
        );

        // Spot-check: decoded pixels agree with direct bilinear samples to
        // the 0.1m encoding step.
        let mut sampler = DemSampler::open(&fixture).unwrap();
        for (i, j) in [(0, 0), (128, 128), (255, 255), (37, 201)] {
            let direct = sampler
                .elevation_at_geo(pixel_center_lon(coord, i), pixel_center_lat(coord, j))
                .unwrap();
            let decoded_e = elevations[j * 256 + i];
            assert!(
                (decoded_e - f64::from(direct)).abs() <= 0.05 + 1e-6,
                "pixel ({i},{j}): sampler {direct}m vs tile {decoded_e}m"
            );
        }
    }

    /// L2 real-data proof for the P6.C3 assembly: the full z5–12 pyramid
    /// over the real DEM packs into a spec-shaped terrain.pmtiles. Run:
    ///   cargo test -p terrain --release -- --ignored --nocapture real_innsbruck
    #[test]
    #[ignore]
    fn real_innsbruck_full_pyramid_assembles() {
        use sample::DemSampler;

        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../offline_sandbox/raw_data/innsbruck_dem.tif");
        let dir = std::env::temp_dir().join(format!("terrain-l2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("terrain.pmtiles");

        let mut sampler = DemSampler::open(&fixture).unwrap();
        let report = archive::build_terrain_archive(
            &mut sampler,
            &out,
            archive::MIN_ZOOM,
            archive::MAX_ZOOM,
        )
        .unwrap();

        // 62 tiles cover the fixture across z5–12 (2+2+2+2+2+4+12+36,
        // independently computed from the slippy formula over the exact
        // 1-arcsec DEM bounds; also pinned by the mercator L1 tests).
        assert_eq!(report.tile_count, 62);
        let bytes = std::fs::read(&out).unwrap();
        assert_eq!(bytes.len() as u64, report.archive_bytes);
        assert_eq!(&bytes[0..7], b"PMTiles");
        assert_eq!(bytes[98], 1, "tile_compression none");
        assert_eq!(bytes[99], 4, "tile_type webp");
        assert_eq!((bytes[100], bytes[101]), (5, 12));

        // The archived z12 Innsbruck tile is byte-identical to a direct
        // render — assembly must not touch payloads.
        let coord = mercator::TileCoord {
            z: 12,
            x: 2177,
            y: 1436,
        };
        let direct = pyramid::render_tile(&mut sampler, coord).unwrap().webp;
        let hay = bytes.windows(direct.len()).any(|w| w == &direct[..]);
        assert!(hay, "direct render not found verbatim in the archive");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// L2 real-data proof for the P6.C4 cursor: the full z5–12 pyramid over
    /// the real DEM, rendered under a realistic slice budget with a FRESH
    /// sampler per slice (process-death simulation), must yield at least
    /// once and land byte-identical to the monolithic build. Run:
    ///   cargo test -p terrain --release -- --ignored --nocapture real_innsbruck
    #[test]
    #[ignore]
    fn real_innsbruck_sliced_run_matches_monolithic() {
        use archive::SliceOutcome;
        use sample::DemSampler;
        use std::time::Duration;

        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../offline_sandbox/raw_data/innsbruck_dem.tif");
        let dir = std::env::temp_dir().join(format!("terrain-l2-slices-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mono = dir.join("mono.pmtiles");
        let sliced = dir.join("sliced.pmtiles");

        let mut sampler = DemSampler::open(&fixture).unwrap();
        archive::build_terrain_archive(&mut sampler, &mono, archive::MIN_ZOOM, archive::MAX_ZOOM)
            .unwrap();

        let budget = Duration::from_millis(50);
        let mut yields = 0u32;
        let report = loop {
            // Fresh sampler per slice: nothing survives but disk state.
            let mut fresh = DemSampler::open(&fixture).unwrap();
            match archive::run_archive_slice(
                &mut fresh,
                &sliced,
                archive::MIN_ZOOM,
                archive::MAX_ZOOM,
                budget,
            )
            .unwrap()
            {
                SliceOutcome::Yielded(_) => yields += 1,
                SliceOutcome::Finished(r) => break r,
            }
            assert!(yields < 1000, "slices stopped making progress");
        };
        assert!(yields > 0, "50ms budget must interrupt a ~4s pyramid");
        assert_eq!(report.tile_count, 62);
        assert_eq!(
            std::fs::read(&mono).unwrap(),
            std::fs::read(&sliced).unwrap(),
            "sliced real-DEM archive must equal the monolithic one"
        );
        println!("real-DEM sliced run: {yields} yields at {budget:?} budget");

        std::fs::remove_dir_all(&dir).ok();
    }
}
