//! Terrain-RGB transformation (Mapbox encoding: base −10000, interval 0.1).
//!
//! `value = (elevation + 10000) * 10`, then
//! `R = (value / 65536) % 256`, `G = (value / 256) % 256`, `B = value % 256`.

use crate::reader::{DemError, DemWindow};

/// Output tile edge length.
pub const TILE_SIZE: usize = 256;

/// Encodes one elevation (metres) into a Terrain-RGB pixel.
///
/// NoData (NaN/±inf) encodes as 0m, the conventional Terrain-RGB fill. The
/// scaled value is rounded — not truncated — so float representation noise
/// (e.g. 573.3 stored as 573.29997) cannot slip one 0.1m step, then clamped
/// to the encodable 24-bit range (−10000m … +1667721.5m).
pub fn elevation_to_rgb(elevation_m: f32) -> [u8; 3] {
    let e = if elevation_m.is_finite() {
        elevation_m
    } else {
        0.0
    };
    let value = ((f64::from(e) + 10_000.0) * 10.0)
        .round()
        .clamp(0.0, 16_777_215.0) as u32;
    [
        ((value / 65_536) % 256) as u8,
        ((value / 256) % 256) as u8,
        (value % 256) as u8,
    ]
}

/// Inverse of [`elevation_to_rgb`], used for verification: exact to 0.1m.
pub fn rgb_to_elevation(rgb: [u8; 3]) -> f64 {
    let value = u32::from(rgb[0]) * 65_536 + u32::from(rgb[1]) * 256 + u32::from(rgb[2]);
    f64::from(value) * 0.1 - 10_000.0
}

/// Transforms a full `TILE_SIZE × TILE_SIZE` elevation grid (row-major, as
/// produced by the pyramid resampler) into an RGB buffer.
pub fn grid_to_terrain_rgb(elevations: &[f32]) -> Result<Vec<u8>, DemError> {
    if elevations.len() != TILE_SIZE * TILE_SIZE {
        return Err(DemError::GridSizeMismatch {
            got: elevations.len(),
        });
    }
    Ok(elevations
        .iter()
        .flat_map(|e| elevation_to_rgb(*e))
        .collect())
}

/// Transforms a decoded DEM window into a `TILE_SIZE × TILE_SIZE` RGB buffer
/// (row-major, 3 bytes/pixel). Edge windows smaller than the tile are padded
/// right/bottom with the NoData pixel (0m); assembly against neighbouring
/// rasters is a later chunk's concern.
pub fn window_to_terrain_rgb(window: &DemWindow) -> Result<Vec<u8>, DemError> {
    if window.width > TILE_SIZE || window.height > TILE_SIZE {
        return Err(DemError::WindowLargerThanTile {
            width: window.width,
            height: window.height,
        });
    }
    let fill = elevation_to_rgb(f32::NAN);
    let mut rgb = vec![0u8; TILE_SIZE * TILE_SIZE * 3];
    for y in 0..TILE_SIZE {
        for x in 0..TILE_SIZE {
            let px = if x < window.width && y < window.height {
                elevation_to_rgb(window.elevations[y * window.width + x])
            } else {
                fill
            };
            rgb[(y * TILE_SIZE + x) * 3..(y * TILE_SIZE + x) * 3 + 3].copy_from_slice(&px);
        }
    }
    Ok(rgb)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_encodings() {
        // Sea level: value 100000 → 1·65536 + 134·256 + 160.
        assert_eq!(elevation_to_rgb(0.0), [1, 134, 160]);
        // Encoding floor: −10000m is pure black.
        assert_eq!(elevation_to_rgb(-10_000.0), [0, 0, 0]);
        // Everest: value 188486 → 2·65536 + 224·256 + 70.
        assert_eq!(elevation_to_rgb(8_848.6), [2, 224, 70]);
        // NoData fills as 0m.
        assert_eq!(elevation_to_rgb(f32::NAN), elevation_to_rgb(0.0));
    }

    #[test]
    fn round_trips_to_a_tenth_of_a_metre() {
        for e in (-1000..45_000).map(|d| f64::from(d) * 0.5 - 500.0) {
            let back = rgb_to_elevation(elevation_to_rgb(e as f32));
            assert!(
                (back - e).abs() < 0.05 + 1e-6,
                "elevation {e} decoded as {back}"
            );
        }
    }

    #[test]
    fn out_of_range_elevations_clamp() {
        assert_eq!(elevation_to_rgb(-20_000.0), [0, 0, 0]);
        assert_eq!(elevation_to_rgb(2e7), [255, 255, 255]);
    }

    #[test]
    fn partial_window_pads_with_nodata_pixel() {
        let window = DemWindow {
            col: 0,
            row: 0,
            width: 2,
            height: 1,
            elevations: vec![100.0, 200.0],
        };
        let rgb = window_to_terrain_rgb(&window).unwrap();
        assert_eq!(rgb.len(), TILE_SIZE * TILE_SIZE * 3);
        assert_eq!(&rgb[0..3], &elevation_to_rgb(100.0));
        assert_eq!(&rgb[3..6], &elevation_to_rgb(200.0));
        // First padded pixel (x=2) and the last pixel are the 0m fill.
        assert_eq!(&rgb[6..9], &elevation_to_rgb(0.0));
        assert_eq!(&rgb[rgb.len() - 3..], &elevation_to_rgb(0.0));
    }

    #[test]
    fn grid_transform_encodes_and_rejects_bad_sizes() {
        let grid = vec![0.0f32; TILE_SIZE * TILE_SIZE];
        let rgb = grid_to_terrain_rgb(&grid).unwrap();
        assert_eq!(rgb.len(), TILE_SIZE * TILE_SIZE * 3);
        assert_eq!(&rgb[0..3], &elevation_to_rgb(0.0));
        assert!(matches!(
            grid_to_terrain_rgb(&[0.0f32; 4]),
            Err(DemError::GridSizeMismatch { got: 4 })
        ));
    }

    #[test]
    fn oversized_window_is_rejected() {
        let window = DemWindow {
            col: 0,
            row: 0,
            width: TILE_SIZE + 1,
            height: 1,
            elevations: vec![0.0; TILE_SIZE + 1],
        };
        assert!(matches!(
            window_to_terrain_rgb(&window),
            Err(DemError::WindowLargerThanTile { .. })
        ));
    }
}
