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

/// The inclusive tile-coordinate rectangle a bounding box intersects at one
/// zoom level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileRange {
    pub z: u8,
    pub x_min: u32,
    pub x_max: u32,
    pub y_min: u32,
    pub y_max: u32,
}

impl TileRange {
    pub fn count(&self) -> u64 {
        u64::from(self.x_max - self.x_min + 1) * u64::from(self.y_max - self.y_min + 1)
    }

    /// Row-major iteration over the range (rendering order is decided by the
    /// caller — assembly re-sorts by Hilbert tile ID).
    pub fn coords(&self) -> impl Iterator<Item = TileCoord> + '_ {
        let z = self.z;
        let xs = self.x_min..=self.x_max;
        (self.y_min..=self.y_max).flat_map(move |y| xs.clone().map(move |x| TileCoord { z, x, y }))
    }
}

/// Tiles intersecting a geographic bounding box (west, south, east, north)
/// at zoom `z`. Edges are handled exclusively on the max side: a box whose
/// east/south edge sits exactly on a tile boundary does NOT pull in the
/// zero-width neighbour. Latitudes clamp to the Mercator limit.
pub fn tile_range_for_bounds(bounds_deg: (f64, f64, f64, f64), z: u8) -> TileRange {
    let (west, south, east, north) = bounds_deg;
    let n = f64::from(1u32 << z);
    let clamp_idx = |v: f64| v.clamp(0.0, n - 1.0) as u32;
    // Normalized Mercator column/row of each edge.
    let xn = |lon: f64| (lon + 180.0) / 360.0 * n;
    let yn = |lat: f64| {
        let rad = lat.clamp(-MAX_LATITUDE_DEG, MAX_LATITUDE_DEG).to_radians();
        (1.0 - (rad.tan() + 1.0 / rad.cos()).ln() / std::f64::consts::PI) / 2.0 * n
    };
    // Max side: step a hair inside the edge so exact-boundary boxes stay
    // exclusive, then never below the min side (degenerate boxes → 1 tile).
    let x_min = clamp_idx(xn(west).floor());
    let y_min = clamp_idx(yn(north).floor());
    TileRange {
        z,
        x_min,
        x_max: clamp_idx((xn(east) - 1e-9).floor()).max(x_min),
        y_min,
        y_max: clamp_idx((yn(south) - 1e-9).floor()).max(y_min),
    }
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

    /// Innsbruck fixture bounds: origin (11.099861…, 47.450139…), 1800×1260
    /// px at exactly 1 arcsec (1/3600°) — the tag stores the full-precision
    /// doubles, not the rounded 0.000278 that tiffinfo prints.
    const DEM_BOUNDS: (f64, f64, f64, f64) = (
        11.099_861_111_136_42,
        47.100_138_888_887_18,
        11.599_861_111_136_486,
        47.450_138_888_887_224,
    );

    #[test]
    fn world_bounds_cover_every_tile() {
        let r = tile_range_for_bounds((-180.0, -85.06, 180.0, 85.06), 0);
        assert_eq!((r.x_min, r.x_max, r.y_min, r.y_max), (0, 0, 0, 0));
        assert_eq!(r.count(), 1);

        let r = tile_range_for_bounds((-180.0, -85.06, 180.0, 85.06), 3);
        assert_eq!((r.x_min, r.x_max, r.y_min, r.y_max), (0, 7, 0, 7));
        assert_eq!(r.count(), 64);
    }

    #[test]
    fn fixture_bounds_enumerate_consistently_across_zooms() {
        for z in 5..=12 {
            let r = tile_range_for_bounds(DEM_BOUNDS, z);
            // Every corner of the box lands inside the range…
            for (lon, lat) in [
                (DEM_BOUNDS.0, DEM_BOUNDS.3),
                (11.5998, 47.1002), // hair inside the east/south edges
            ] {
                let t = tile_containing(lon, lat, z);
                assert!(
                    r.x_min <= t.x && t.x <= r.x_max && r.y_min <= t.y && t.y <= r.y_max,
                    "z{z}: corner tile {t} outside range {r:?}"
                );
            }
            // …and every enumerated tile actually intersects the box.
            for t in r.coords() {
                let (lon_min, lat_min, lon_max, lat_max) = tile_bounds_deg(t);
                assert!(
                    lon_max > DEM_BOUNDS.0
                        && lon_min < DEM_BOUNDS.2
                        && lat_max > DEM_BOUNDS.1
                        && lat_min < DEM_BOUNDS.3,
                    "z{z}: tile {t} does not intersect the DEM box"
                );
            }
        }
        // Known z12 extent, cross-checked against the slippy formula.
        let r = tile_range_for_bounds(DEM_BOUNDS, 12);
        assert_eq!((r.x_min, r.x_max), (2174, 2179));
        assert_eq!((r.y_min, r.y_max), (1433, 1438));
        // Full z5–12 pyramid over the fixture: 62 tiles (2+2+2+2+2+4+12+36),
        // the count the L2 assembly test pins against the real archive.
        let total: u64 = (5..=12)
            .map(|z| tile_range_for_bounds(DEM_BOUNDS, z).count())
            .sum();
        assert_eq!(total, 62);
        // z5 collapses to the small known window.
        let r = tile_range_for_bounds(DEM_BOUNDS, 5);
        assert_eq!((r.x_min, r.x_max, r.y_min, r.y_max), (16, 17, 11, 11));
    }

    #[test]
    fn exact_tile_boundaries_stay_exclusive_on_the_max_side() {
        // The z1 tile (1,0) spans lon 0..180: a box ending exactly at lon 0
        // must not include it.
        let r = tile_range_for_bounds((-90.0, 10.0, 0.0, 40.0), 1);
        assert_eq!((r.x_min, r.x_max), (0, 0));
        // A degenerate (point) box still yields one tile.
        let r = tile_range_for_bounds((11.39, 47.26, 11.39, 47.26), 12);
        assert_eq!(r.count(), 1);
        assert_eq!((r.x_min, r.y_min), (2177, 1436));
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
