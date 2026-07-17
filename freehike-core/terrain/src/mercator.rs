//! WebMercator (slippy-map) tile arithmetic.
//!
//! Tiles address the EPSG:3857 plane, but every function here speaks
//! geographic degrees (EPSG:4326) because that is the fixture DEM's model
//! space — the projection nonlinearity lives entirely in the latitude
//! formula. Longitude is linear across a tile; latitude is NOT, so per-row
//! latitudes must come from the Mercator inverse, never from interpolating
//! the tile's corner latitudes.

use crate::rgb::TILE_SIZE;

/// Mercator's latitude clamp: atan(sinh(π)) — the square-world limit.
pub const MAX_LATITUDE_DEG: f64 = 85.051_128_779_806_59;

/// One WebMercator tile address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TileCoord {
    pub z: u8,
    pub x: u32,
    pub y: u32,
}

impl TileCoord {
    /// Tiles per axis at this zoom.
    pub fn tiles_across(self) -> u32 {
        1u32 << self.z
    }
}

impl std::fmt::Display for TileCoord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}/{}", self.z, self.x, self.y)
    }
}

/// Latitude (degrees) of a normalized Mercator row `yn` ∈ [0,1], 0 = north.
fn lat_of_normalized(yn: f64) -> f64 {
    (std::f64::consts::PI * (1.0 - 2.0 * yn))
        .sinh()
        .atan()
        .to_degrees()
}

/// Longitude (degrees) of a normalized Mercator column `xn` ∈ [0,1].
fn lon_of_normalized(xn: f64) -> f64 {
    xn * 360.0 - 180.0
}

/// Geographic bounding box of a tile: (lon_min, lat_min, lon_max, lat_max).
pub fn tile_bounds_deg(t: TileCoord) -> (f64, f64, f64, f64) {
    let n = f64::from(t.tiles_across());
    (
        lon_of_normalized(f64::from(t.x) / n),
        lat_of_normalized(f64::from(t.y + 1) / n),
        lon_of_normalized(f64::from(t.x + 1) / n),
        lat_of_normalized(f64::from(t.y) / n),
    )
}

/// Longitude of pixel-column `i`'s center in a 256px tile.
pub fn pixel_center_lon(t: TileCoord, i: usize) -> f64 {
    let n = f64::from(t.tiles_across());
    lon_of_normalized((f64::from(t.x) + (i as f64 + 0.5) / TILE_SIZE as f64) / n)
}

/// Latitude of pixel-row `j`'s center in a 256px tile.
pub fn pixel_center_lat(t: TileCoord, j: usize) -> f64 {
    let n = f64::from(t.tiles_across());
    lat_of_normalized((f64::from(t.y) + (j as f64 + 0.5) / TILE_SIZE as f64) / n)
}

/// The tile containing a geographic point at zoom `z`. Latitude is clamped
/// to the Mercator limit; indices clamp to the last tile on the east/south
/// edges so `lon = 180` stays addressable.
pub fn tile_containing(lon: f64, lat: f64, z: u8) -> TileCoord {
    let n = f64::from(1u32 << z);
    let lat = lat.clamp(-MAX_LATITUDE_DEG, MAX_LATITUDE_DEG);
    let xn = (lon + 180.0) / 360.0;
    let lat_rad = lat.to_radians();
    let yn = (1.0 - (lat_rad.tan() + 1.0 / lat_rad.cos()).ln() / std::f64::consts::PI) / 2.0;
    let clamp_axis = |v: f64| (v * n).floor().clamp(0.0, n - 1.0) as u32;
    TileCoord {
        z,
        x: clamp_axis(xn),
        y: clamp_axis(yn),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn z0_tile_spans_the_mercator_world() {
        let (lon_min, lat_min, lon_max, lat_max) = tile_bounds_deg(TileCoord { z: 0, x: 0, y: 0 });
        assert_eq!((lon_min, lon_max), (-180.0, 180.0));
        assert!((lat_max - MAX_LATITUDE_DEG).abs() < 1e-9);
        assert!((lat_min + MAX_LATITUDE_DEG).abs() < 1e-9);
    }

    #[test]
    fn innsbruck_lands_in_the_known_z12_tile() {
        // Innsbruck city center; the tile numbers cross-checked against the
        // standard slippy-map formula.
        let t = tile_containing(11.39, 47.26, 12);
        assert_eq!(
            t,
            TileCoord {
                z: 12,
                x: 2177,
                y: 1436
            }
        );

        // The point sits inside that tile's bounds.
        let (lon_min, lat_min, lon_max, lat_max) = tile_bounds_deg(t);
        assert!(lon_min <= 11.39 && 11.39 < lon_max);
        assert!(lat_min <= 47.26 && 47.26 < lat_max);
    }

    #[test]
    fn pixel_centers_stay_inside_bounds_and_are_monotonic() {
        let t = TileCoord {
            z: 12,
            x: 2177,
            y: 1436,
        };
        let (lon_min, lat_min, lon_max, lat_max) = tile_bounds_deg(t);

        let first_lat = pixel_center_lat(t, 0);
        let last_lat = pixel_center_lat(t, TILE_SIZE - 1);
        assert!(first_lat < lat_max && first_lat > last_lat && last_lat > lat_min);

        let first_lon = pixel_center_lon(t, 0);
        let last_lon = pixel_center_lon(t, TILE_SIZE - 1);
        assert!(first_lon > lon_min && first_lon < last_lon && last_lon < lon_max);

        // Rows are strictly monotonic all the way down (Mercator inverse,
        // not a linear ramp).
        for j in 1..TILE_SIZE {
            assert!(pixel_center_lat(t, j) < pixel_center_lat(t, j - 1));
        }
    }

    #[test]
    fn poles_and_dateline_clamp_into_valid_tiles() {
        let n = 1u32 << 5;
        let t = tile_containing(180.0, -89.9, 5);
        assert_eq!((t.x, t.y), (n - 1, n - 1));
        let t = tile_containing(-180.0, 89.9, 5);
        assert_eq!((t.x, t.y), (0, 0));
    }
}
