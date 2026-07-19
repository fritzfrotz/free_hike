// SPDX-License-Identifier: Apache-2.0
//! Tile-pyramid rendering: WebMercator (z,x,y) → Terrain-RGB WebP.
//!
//! Per output pixel the sampler is asked for the elevation at that pixel's
//! geographic center — one Mercator-inverse latitude per row, linear
//! longitudes per column. Bilinear resampling handles both directions of
//! scale mismatch: z12 slightly oversamples the ~1-arcsec DEM smoothly, and
//! low zooms downsample without nearest-neighbour terracing (their residual
//! aliasing is acceptable for hillshading; a proper reduction filter is a
//! later trade study if z5–8 renders look noisy).

use std::io::{Read, Seek};
use std::path::Path;

use crate::mercator::{pixel_center_lat, pixel_center_lon, TileCoord};
use crate::reader::DemError;
use crate::rgb::{self, TILE_SIZE};
use crate::sample::DemSampler;
use crate::{webp, TerrainError};

/// One rendered pyramid tile.
pub struct RenderedTile {
    pub coord: TileCoord,
    pub webp: Vec<u8>,
}

/// Samples the 256×256 pixel-center elevation grid for a tile. Pixels beyond
/// the DEM extent come back NaN (→ 0m in Terrain-RGB).
pub fn sample_tile_elevations<R: Read + Seek>(
    sampler: &mut DemSampler<R>,
    coord: TileCoord,
) -> Result<Vec<f32>, DemError> {
    let mut grid = Vec::with_capacity(TILE_SIZE * TILE_SIZE);
    for j in 0..TILE_SIZE {
        let lat = pixel_center_lat(coord, j);
        for i in 0..TILE_SIZE {
            grid.push(sampler.elevation_at_geo(pixel_center_lon(coord, i), lat)?);
        }
    }
    Ok(grid)
}

/// Full per-tile pipeline: resample → Terrain-RGB → lossless WebP.
pub fn render_tile<R: Read + Seek>(
    sampler: &mut DemSampler<R>,
    coord: TileCoord,
) -> Result<RenderedTile, TerrainError> {
    let grid = sample_tile_elevations(sampler, coord)?;
    let rgb_buf = rgb::grid_to_terrain_rgb(&grid)?;
    let webp = webp::encode_rgb_lossless(&rgb_buf, TILE_SIZE as u32, TILE_SIZE as u32)?;
    Ok(RenderedTile { coord, webp })
}

/// One-shot convenience: open the DEM and render a single (z,x,y) tile.
pub fn dem_tile_to_webp(dem_path: &Path, coord: TileCoord) -> Result<RenderedTile, TerrainError> {
    let mut sampler = DemSampler::open(dem_path)?;
    render_tile(&mut sampler, coord)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader::{test_dem, WindowedDemReader};
    use crate::rgb::rgb_to_elevation;

    /// Innsbruck-shaped synthetic DEM: same origin as the real fixture but a
    /// coarse 0.01° grid, 60×40 → lon 11.0999…11.6999, lat 47.0501…47.4501.
    /// Elevation ramp 500 + 10x + 5y (500…1285m).
    fn sampler() -> DemSampler<std::io::Cursor<Vec<u8>>> {
        let dem = test_dem::build(60, 40, 9999, Some((11.099861, 47.450139, 0.01)), |x, y| {
            (500 + 10 * x + 5 * y) as u16
        });
        DemSampler::new(WindowedDemReader::new(dem).unwrap()).unwrap()
    }

    #[test]
    fn known_innsbruck_tile_renders_valid_webp() {
        // The known z12 tile over Innsbruck (see mercator tests), fully
        // inside the synthetic DEM's bounds.
        let coord = TileCoord {
            z: 12,
            x: 2177,
            y: 1436,
        };
        let tile = render_tile(&mut sampler(), coord).unwrap();

        assert_eq!(tile.coord, coord);
        assert_eq!(&tile.webp[0..4], b"RIFF");
        assert_eq!(&tile.webp[8..12], b"WEBP");

        // Decode and verify every pixel carries a plausible ramp elevation —
        // an all-inside tile must contain no 0m NoData fill.
        let decoded = image::load_from_memory(&tile.webp).unwrap().into_rgb8();
        assert_eq!(decoded.dimensions(), (TILE_SIZE as u32, TILE_SIZE as u32));
        for px in decoded.pixels() {
            let e = rgb_to_elevation(px.0);
            assert!((500.0..=1285.0).contains(&e), "elevation {e} off the ramp");
        }
    }

    #[test]
    fn low_zoom_tile_fills_outside_coverage_with_nodata() {
        // z5 tile containing Innsbruck spans lon 11.25…22.5 — almost all of
        // it lies beyond the DEM, which must fill as 0m, while the covered
        // northwest corner still carries ramp elevations.
        let coord = TileCoord { z: 5, x: 17, y: 11 };
        let tile = render_tile(&mut sampler(), coord).unwrap();
        let decoded = image::load_from_memory(&tile.webp).unwrap().into_rgb8();

        let elevations: Vec<f64> = decoded.pixels().map(|px| rgb_to_elevation(px.0)).collect();
        let fill = elevations.iter().filter(|e| **e == 0.0).count();
        let covered = elevations.iter().filter(|e| **e >= 500.0).count();
        assert!(fill > covered, "expected mostly fill: {fill} vs {covered}");
        assert!(covered > 0, "DEM corner must still be sampled");
    }

    #[test]
    fn oversampling_interpolates_instead_of_terracing() {
        // At z12 one output pixel is far finer than the synthetic 0.01°
        // grid; a row crossing the ramp must produce many distinct values,
        // not nearest-neighbour steps of exactly 10m.
        let coord = TileCoord {
            z: 12,
            x: 2177,
            y: 1436,
        };
        let grid = sample_tile_elevations(&mut sampler(), coord).unwrap();
        let row = &grid[0..TILE_SIZE];
        let distinct = {
            let mut v: Vec<i64> = row.iter().map(|e| (e * 1000.0) as i64).collect();
            v.sort_unstable();
            v.dedup();
            v.len()
        };
        assert!(distinct > 50, "row collapsed to {distinct} levels");
    }
}
