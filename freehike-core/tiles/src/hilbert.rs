// SPDX-License-Identifier: Apache-2.0
//! PMTiles tile IDs: cumulative-pyramid offset + Hilbert curve position.
//!
//! The PMTiles v3 spec addresses every tile in the z0..=z pyramid with a
//! single u64: `tile_id = (number of tiles in all zooms below z) + (Hilbert
//! curve index of (x, y) at zoom z)`. Directories are sorted by this ID, and
//! a "clustered" archive stores tile data in the same order — which is the
//! whole point: spatially adjacent tiles land in adjacent bytes, so a
//! renderer panning the map reads runs, not scatter.
//!
//! The Hilbert orientation below matches the reference `go-pmtiles`
//! implementation (rotation uses the current sub-square size `s`); the unit
//! tests pin the spec's published example IDs so a silent orientation flip
//! can never ship.

/// Zooms above this overflow the u64 tile-ID space partitioning used here.
/// (The spec itself caps directories at z31; our pipeline tops out at z14.)
pub const MAX_ID_ZOOM: u8 = 31;

/// Number of tiles in all zoom levels strictly below `zoom`:
/// `sum(4^i for i < zoom) = (4^zoom - 1) / 3`.
fn zoom_base(zoom: u8) -> u64 {
    ((1u64 << (2 * u32::from(zoom))) - 1) / 3
}

/// Rotate/flip a sub-square of size `n` (the standard Hilbert step).
fn rotate(n: u64, x: &mut u64, y: &mut u64, rx: u64, ry: u64) {
    if ry == 0 {
        if rx == 1 {
            *x = n - 1 - *x;
            *y = n - 1 - *y;
        }
        std::mem::swap(x, y);
    }
}

/// PMTiles tile ID for `(zoom, x, y)` (XYZ scheme, y grows downward).
///
/// # Panics
/// If `zoom > MAX_ID_ZOOM` or `x`/`y` lie outside the zoom's grid — both are
/// internal invariant violations (keys come from our own tile index), never
/// hostile input.
pub fn tile_id(zoom: u8, x: u32, y: u32) -> u64 {
    assert!(zoom <= MAX_ID_ZOOM, "zoom {zoom} exceeds tile-ID space");
    let n = 1u64 << zoom;
    let (mut tx, mut ty) = (u64::from(x), u64::from(y));
    assert!(tx < n && ty < n, "tile ({x},{y}) outside z{zoom} grid");

    let mut d: u64 = 0;
    let mut s = n / 2;
    while s > 0 {
        let rx = u64::from(tx & s > 0);
        let ry = u64::from(ty & s > 0);
        d += s * s * ((3 * rx) ^ ry);
        // Drop the just-processed bit (and anything above) before the
        // sub-square reflection: `s-1-x` on the classic algorithm relies on
        // wrapping arithmetic when high bits are still set — the bits below
        // `s` come out identical either way, and only those are ever read
        // again, but masking first keeps the math in-range (no debug-build
        // overflow panic).
        tx &= s - 1;
        ty &= s - 1;
        rotate(s, &mut tx, &mut ty, rx, ry);
        s /= 2;
    }
    zoom_base(zoom) + d
}

/// Inverse of [`tile_id`]: recovers `(zoom, x, y)` from a PMTiles tile ID.
/// `None` if the ID lies beyond the [`MAX_ID_ZOOM`] pyramid (a corrupt
/// cursor, not a reachable state).
pub fn tile_id_to_zxy(id: u64) -> Option<(u8, u32, u32)> {
    let mut zoom = 0u8;
    while zoom < MAX_ID_ZOOM && id >= zoom_base(zoom + 1) {
        zoom += 1;
    }
    let d = id - zoom_base(zoom);
    let n = 1u64 << zoom;
    if d >= n * n {
        return None; // only possible at MAX_ID_ZOOM overflow
    }

    let (mut x, mut y) = (0u64, 0u64);
    let mut t = d;
    let mut s = 1u64;
    while s < n {
        let rx = 1 & (t / 2);
        let ry = 1 & (t ^ rx);
        rotate(s, &mut x, &mut y, rx, ry);
        x += s * rx;
        y += s * ry;
        t /= 4;
        s *= 2;
    }
    Some((zoom, x as u32, y as u32))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The spec's published example IDs: z0 is 0; z1 walks the Hilbert
    /// order (0,0),(0,1),(1,1),(1,0) as IDs 1..=4; z2 starts at 5; and the
    /// z2 curve's first quadrant is (0,0),(1,0),(1,1),(0,1) → (1,1) is 7.
    #[test]
    fn spec_example_ids() {
        assert_eq!(tile_id(0, 0, 0), 0);
        assert_eq!(tile_id(1, 0, 0), 1);
        assert_eq!(tile_id(1, 0, 1), 2);
        assert_eq!(tile_id(1, 1, 1), 3);
        assert_eq!(tile_id(1, 1, 0), 4);
        assert_eq!(tile_id(2, 0, 0), 5);
        assert_eq!(tile_id(2, 1, 0), 6);
        assert_eq!(tile_id(2, 1, 1), 7);
    }

    #[test]
    fn zoom_base_offsets() {
        assert_eq!(zoom_base(0), 0);
        assert_eq!(zoom_base(1), 1);
        assert_eq!(zoom_base(2), 5);
        assert_eq!(zoom_base(3), 21);
        // z14 pyramid base: sum of 4^i for i<14.
        assert_eq!(zoom_base(14), (4u64.pow(14) - 1) / 3);
    }

    /// Exhaustive inverse check across small zooms: every cell roundtrips,
    /// and IDs within a zoom are a bijection onto the zoom's range.
    #[test]
    fn roundtrips_exhaustively_at_small_zooms() {
        for z in 0..=5u8 {
            let n = 1u32 << z;
            let mut seen = std::collections::BTreeSet::new();
            for x in 0..n {
                for y in 0..n {
                    let id = tile_id(z, x, y);
                    assert!(id >= zoom_base(z) && id < zoom_base(z + 1));
                    assert!(seen.insert(id), "duplicate id {id} at z{z}");
                    assert_eq!(tile_id_to_zxy(id), Some((z, x, y)));
                }
            }
            assert_eq!(seen.len() as u64, u64::from(n) * u64::from(n));
        }
    }

    #[test]
    fn roundtrips_at_base_tile_zoom() {
        // Innsbruck-ish z14 tile and the grid corners.
        for &(x, y) in &[(8703u32, 5747u32), (0, 0), (16383, 16383), (16383, 0)] {
            let id = tile_id(14, x, y);
            assert_eq!(tile_id_to_zxy(id), Some((14, x, y)));
        }
    }

    /// Hilbert adjacency: consecutive IDs at a zoom are neighbouring cells
    /// (the locality property clustering relies on).
    #[test]
    fn consecutive_ids_are_grid_neighbours() {
        let z = 4u8;
        let n = 1u32 << z;
        let mut by_id = std::collections::BTreeMap::new();
        for x in 0..n {
            for y in 0..n {
                by_id.insert(tile_id(z, x, y), (x, y));
            }
        }
        let cells: Vec<_> = by_id.values().copied().collect();
        for pair in cells.windows(2) {
            let dx = pair[0].0.abs_diff(pair[1].0);
            let dy = pair[0].1.abs_diff(pair[1].1);
            assert_eq!(
                dx + dy,
                1,
                "curve jumped from {:?} to {:?}",
                pair[0],
                pair[1]
            );
        }
    }
}
