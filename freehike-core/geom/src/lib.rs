//! `geom` — pure geometry math for Phase 4 (simplification, clipping,
//! and slippy-tile grid math).
//!
//! Web Mercator meters (EPSG:3857) in, Web Mercator meters out.
//! Dependency-free: the pipeline already speaks `Vec<(f64, f64)>`
//! (`pbf::assemble_way_geometry`'s output), and every crate here must
//! cross-compile to aarch64 iOS/Android — smallest possible tree wins.
//!
//! References: research/On-Device Map Compiler Blueprint.pdf
//! ("Geospatial Transformation: Projection, Simplification, and
//! Clipping"), md master plan Phase 4.

use std::collections::BTreeSet;

/// Ramer-Douglas-Peucker polyline simplification.
///
/// - `linestring` is an ordered Web-Mercator polyline (meters, EPSG:3857).
/// - `epsilon` is the maximum perpendicular deviation in meters; callers
///   derive it per zoom level (coarse at z5, ~0 at z14+).
/// - The first and last vertices are always preserved.
/// - Inputs with fewer than 3 vertices are returned unchanged.
///
/// Iterative: an explicit heap-allocated stack of `(start, end)` index
/// ranges stands in for the call stack a naive recursive RDP would use.
/// Real OSM ways reach tens of thousands of vertices, and this runs
/// inside the mobile FFI loop's constrained thread — native recursion
/// depth there is not something we can blow.
pub fn simplify_rdp(linestring: &[(f64, f64)], epsilon: f64) -> Vec<(f64, f64)> {
    let n = linestring.len();
    if n < 3 {
        return linestring.to_vec();
    }

    let mut keep = vec![false; n];
    keep[0] = true;
    keep[n - 1] = true;

    let mut stack: Vec<(usize, usize)> = vec![(0, n - 1)];

    while let Some((start, end)) = stack.pop() {
        if end <= start + 1 {
            continue;
        }

        let (split_idx, max_dist) = farthest_point(linestring, start, end);

        if max_dist > epsilon {
            keep[split_idx] = true;
            stack.push((start, split_idx));
            stack.push((split_idx, end));
        }
    }

    linestring
        .iter()
        .zip(keep.iter())
        .filter_map(|(&p, &k)| k.then_some(p))
        .collect()
}

/// Index and perpendicular distance of the point in `(start, end)`
/// (exclusive of the endpoints) farthest from the line through
/// `linestring[start]` and `linestring[end]`.
fn farthest_point(linestring: &[(f64, f64)], start: usize, end: usize) -> (usize, f64) {
    let (x1, y1) = linestring[start];
    let (x2, y2) = linestring[end];
    let dx = x2 - x1;
    let dy = y2 - y1;
    let seg_len = (dx * dx + dy * dy).sqrt();

    let mut best_idx = start + 1;
    let mut best_dist = -1.0_f64;

    for (i, &(px, py)) in linestring.iter().enumerate().take(end).skip(start + 1) {
        let dist = if seg_len == 0.0 {
            // start and end coincide: the "line" is a point.
            ((px - x1).powi(2) + (py - y1).powi(2)).sqrt()
        } else {
            (dy * px - dx * py + x2 * y1 - y2 * x1).abs() / seg_len
        };

        if dist > best_dist {
            best_dist = dist;
            best_idx = i;
        }
    }

    (best_idx, best_dist)
}

/// Liang-Barsky clip of a linestring to an axis-aligned tile box.
///
/// - `bounds` is `(min_x, min_y, max_x, max_y)` in Web-Mercator meters —
///   a tile's extent plus buffer, computed by the binning stage.
/// - Every segment is clipped independently against the box's four
///   half-planes; boundary intersections are computed exactly and
///   inserted as new vertices (never merely dropped).
/// - A linestring that exits and re-enters the box produces multiple
///   disjoint runs of vertices. That's why the return type is
///   `Vec<Vec<(f64, f64)>>` rather than a single polyline: one `Vec`
///   cannot represent two unconnected runs without a phantom line
///   bridging the gap between them.
/// - Degenerate clips that collapse to a single point are dropped; only
///   runs of 2+ vertices are emitted.
pub fn clip_to_bounds(
    linestring: &[(f64, f64)],
    bounds: (f64, f64, f64, f64),
) -> Vec<Vec<(f64, f64)>> {
    let mut result = Vec::new();
    if linestring.len() < 2 {
        return result;
    }

    let mut current: Vec<(f64, f64)> = Vec::new();

    for pair in linestring.windows(2) {
        let (a, b) = (pair[0], pair[1]);

        match clip_segment(a, b, bounds) {
            Some((c0, c1)) => {
                if current.last() != Some(&c0) {
                    flush(&mut current, &mut result);
                    current.push(c0);
                }
                current.push(c1);
            }
            None => flush(&mut current, &mut result),
        }
    }

    flush(&mut current, &mut result);
    result
}

/// Move `current` into `result` if it's a real (2+ vertex, nonzero
/// length) run, otherwise discard it. Either way `current` is empty
/// afterward.
///
/// The nonzero-length check matters at tangential corner touches: the
/// clip can degenerate to a single point duplicated as both endpoints
/// of a segment (see `clip_touching_corner_tangentially_drops_degenerate_point`),
/// which satisfies `len() >= 2` without representing any real line.
fn flush(current: &mut Vec<(f64, f64)>, result: &mut Vec<Vec<(f64, f64)>>) {
    let has_length = current.windows(2).any(|w| w[0] != w[1]);
    if current.len() >= 2 && has_length {
        result.push(std::mem::take(current));
    } else {
        current.clear();
    }
}

/// Liang-Barsky parametric clip of segment `a`-`b` against `bounds`.
/// Returns the clipped endpoints, or `None` if the segment does not
/// intersect the box at all.
///
/// Endpoints that aren't actually clipped come back bit-identical to
/// the input (see `at`'s `t <= 0.0` / `t >= 1.0` short circuits): when
/// two adjacent input segments both lie inside the box, the shared
/// vertex between them round-trips as the exact same value in both
/// calls, so `clip_to_bounds` can detect "still contiguous" with a
/// plain `==` instead of a tolerance comparison.
fn clip_segment(
    a: (f64, f64),
    b: (f64, f64),
    bounds: (f64, f64, f64, f64),
) -> Option<((f64, f64), (f64, f64))> {
    let (min_x, min_y, max_x, max_y) = bounds;
    let (x0, y0) = a;
    let (x1, y1) = b;
    let dx = x1 - x0;
    let dy = y1 - y0;

    let mut t0 = 0.0_f64;
    let mut t1 = 1.0_f64;

    // (p, q) per boundary, in the classic Liang-Barsky order:
    // left, right, bottom, top.
    let checks = [
        (-dx, x0 - min_x),
        (dx, max_x - x0),
        (-dy, y0 - min_y),
        (dy, max_y - y0),
    ];

    for (p, q) in checks {
        if p == 0.0 {
            // Segment is parallel to this boundary. Outside it entirely
            // (q < 0) means it never intersects the box at all.
            if q < 0.0 {
                return None;
            }
        } else {
            let r = q / p;
            if p < 0.0 {
                // Entering half-plane: tighten the lower bound.
                if r > t1 {
                    return None;
                }
                if r > t0 {
                    t0 = r;
                }
            } else {
                // Exiting half-plane: tighten the upper bound.
                if r < t0 {
                    return None;
                }
                if r < t1 {
                    t1 = r;
                }
            }
        }
    }

    if t0 > t1 {
        return None;
    }

    Some((at(a, b, dx, dy, t0), at(a, b, dx, dy, t1)))
}

/// Point at parameter `t` along `a`-`b`, snapping exactly to the
/// original endpoint at `t <= 0.0` / `t >= 1.0` rather than
/// recomputing it through interpolation.
fn at(a: (f64, f64), b: (f64, f64), dx: f64, dy: f64, t: f64) -> (f64, f64) {
    if t <= 0.0 {
        a
    } else if t >= 1.0 {
        b
    } else {
        (a.0 + t * dx, a.1 + t * dy)
    }
}

// ---------------------------------------------------------------------------
// Slippy-tile grid math (XYZ convention: x rightward, y downward from the
// north-west corner of the Web Mercator square)
// ---------------------------------------------------------------------------

/// Half the side of the Web Mercator world square, in meters:
/// `R * π` for the WGS84 equatorial radius. The projected world spans
/// `[-HALF, +HALF]` on both axes.
pub const MERCATOR_HALF_WORLD_M: f64 = 20_037_508.342_789_244;

/// All tile functions take a zoom in this range: `2^zoom` must fit the grid
/// arithmetic and `u32` tile coordinates. Real jobs use z0-z16.
pub const MAX_TILE_ZOOM: u8 = 30;

/// Side length of one tile at `zoom`, in Web Mercator meters.
pub fn tile_extent_m(zoom: u8) -> f64 {
    debug_assert!(zoom <= MAX_TILE_ZOOM);
    (2.0 * MERCATOR_HALF_WORLD_M) / (1u64 << zoom.min(MAX_TILE_ZOOM)) as f64
}

/// Ground resolution of one 256px-tile pixel at `zoom` (at the equator).
pub fn meters_per_pixel(zoom: u8) -> f64 {
    tile_extent_m(zoom) / 256.0
}

/// RDP epsilon for geometry rendered at `zoom`: half a display pixel.
/// Deviations under half a pixel are invisible, so this is the smallest
/// epsilon worth simplifying with — aggressive at overview zooms (~2.4km
/// at z5), imperceptible at the z14 base tiles (~4.8m), per the Blueprint's
/// "aggressive at z5, minimized at z14" scaling.
pub fn epsilon_for_zoom(zoom: u8) -> f64 {
    meters_per_pixel(zoom) / 2.0
}

/// The tile containing a Web Mercator point at `zoom`. Points outside the
/// world square clamp into the edge tiles (the projection already clamps
/// latitude, so only x can legitimately sit on the ±180° seam).
pub fn mercator_to_tile(x: f64, y: f64, zoom: u8) -> (u32, u32) {
    let extent = tile_extent_m(zoom);
    let n = 1i64 << zoom.min(MAX_TILE_ZOOM);
    let tx = (((x + MERCATOR_HALF_WORLD_M) / extent).floor() as i64).clamp(0, n - 1);
    let ty = (((MERCATOR_HALF_WORLD_M - y) / extent).floor() as i64).clamp(0, n - 1);
    (tx as u32, ty as u32)
}

/// Web Mercator bounds `(min_x, min_y, max_x, max_y)` of tile `(tx, ty)`
/// at `zoom` — the exact box, no buffer (callers add their own rendering
/// buffer before clipping).
pub fn tile_bounds(zoom: u8, tx: u32, ty: u32) -> (f64, f64, f64, f64) {
    let extent = tile_extent_m(zoom);
    let min_x = tx as f64 * extent - MERCATOR_HALF_WORLD_M;
    let max_y = MERCATOR_HALF_WORLD_M - ty as f64 * extent;
    (min_x, max_y - extent, min_x + extent, max_y)
}

/// Inserts into `out` every tile the segment `a`-`b` passes through at
/// `zoom`, via Amanatides-Woo grid traversal.
///
/// This is O(tiles actually crossed), NOT O(bounding-box area): a long
/// diagonal segment (extract-clipped ways can jump across the whole bbox)
/// crosses `|Δtx| + |Δty| + 1` tiles, while its bounding box can cover
/// millions — scanning the box would be a denial-of-service on the device.
/// Both endpoint tiles are always included, and the traversal is bounded by
/// the cell distance, so float noise can neither hang the loop nor drop an
/// endpoint.
pub fn tiles_crossed_by_segment(
    a: (f64, f64),
    b: (f64, f64),
    zoom: u8,
    out: &mut BTreeSet<(u32, u32)>,
) {
    let extent = tile_extent_m(zoom);
    let n = 1i64 << zoom.min(MAX_TILE_ZOOM);
    let clamp_cell = |c: f64| (c.floor() as i64).clamp(0, n - 1);

    // Grid space: u rightward, v downward, one unit per tile.
    let (u0, v0) = (
        (a.0 + MERCATOR_HALF_WORLD_M) / extent,
        (MERCATOR_HALF_WORLD_M - a.1) / extent,
    );
    let (u1, v1) = (
        (b.0 + MERCATOR_HALF_WORLD_M) / extent,
        (MERCATOR_HALF_WORLD_M - b.1) / extent,
    );

    let (mut cx, mut cy) = (clamp_cell(u0), clamp_cell(v0));
    let (ex, ey) = (clamp_cell(u1), clamp_cell(v1));
    out.insert((cx as u32, cy as u32));
    out.insert((ex as u32, ey as u32));

    let du = u1 - u0;
    let dv = v1 - v0;
    let step_x: i64 = if du >= 0.0 { 1 } else { -1 };
    let step_y: i64 = if dv >= 0.0 { 1 } else { -1 };
    // Parametric distance (in units of the segment) to the first grid line
    // on each axis, then per-cell increments.
    let mut t_max_x = if du == 0.0 {
        f64::INFINITY
    } else {
        let boundary = if du > 0.0 { (cx + 1) as f64 } else { cx as f64 };
        (boundary - u0) / du
    };
    let mut t_max_y = if dv == 0.0 {
        f64::INFINITY
    } else {
        let boundary = if dv > 0.0 { (cy + 1) as f64 } else { cy as f64 };
        (boundary - v0) / dv
    };
    let t_delta_x = if du == 0.0 {
        f64::INFINITY
    } else {
        (1.0 / du).abs()
    };
    let t_delta_y = if dv == 0.0 {
        f64::INFINITY
    } else {
        (1.0 / dv).abs()
    };

    let max_steps = (ex - cx).abs() + (ey - cy).abs();
    for _ in 0..max_steps {
        if cx == ex && cy == ey {
            break;
        }
        if t_max_x < t_max_y {
            t_max_x += t_delta_x;
            cx = (cx + step_x).clamp(0, n - 1);
        } else {
            t_max_y += t_delta_y;
            cy = (cy + step_y).clamp(0, n - 1);
        }
        out.insert((cx as u32, cy as u32));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- simplify_rdp ----

    #[test]
    fn rdp_empty_and_single_point_are_unchanged() {
        let empty: Vec<(f64, f64)> = vec![];
        assert_eq!(simplify_rdp(&empty, 1.0), empty);

        let one = vec![(1.0, 2.0)];
        assert_eq!(simplify_rdp(&one, 1.0), one);
    }

    #[test]
    fn rdp_two_point_line_is_unchanged() {
        let line = vec![(0.0, 0.0), (10.0, 10.0)];
        assert_eq!(simplify_rdp(&line, 0.001), line);
    }

    #[test]
    fn rdp_removes_collinear_points() {
        let line = vec![
            (0.0, 0.0),
            (1.0, 1.0),
            (2.0, 2.0),
            (3.0, 3.0),
            (4.0, 4.0),
            (5.0, 5.0),
        ];
        assert_eq!(simplify_rdp(&line, 0.5), vec![(0.0, 0.0), (5.0, 5.0)]);
    }

    #[test]
    fn rdp_preserves_sharp_corners() {
        // A right-angle detour that a tight epsilon must keep.
        let line = vec![(0.0, 0.0), (5.0, 0.0), (5.0, 10.0), (10.0, 10.0)];
        let simplified = simplify_rdp(&line, 0.5);
        assert_eq!(simplified, line);
    }

    #[test]
    fn rdp_drops_small_wiggle_within_epsilon() {
        // Midpoint deviates by exactly 1.0 from the straight chord.
        let line = vec![(0.0, 0.0), (5.0, 1.0), (10.0, 0.0)];
        assert_eq!(simplify_rdp(&line, 2.0), vec![(0.0, 0.0), (10.0, 0.0)]);
        // A tighter epsilon must keep the deviating point.
        assert_eq!(
            simplify_rdp(&line, 0.5),
            vec![(0.0, 0.0), (5.0, 1.0), (10.0, 0.0)]
        );
    }

    #[test]
    fn rdp_endpoints_always_survive() {
        let line = vec![(0.0, 0.0), (1.0, 0.0), (2.0, 0.0), (3.0, 0.0)];
        let simplified = simplify_rdp(&line, 1000.0);
        assert_eq!(simplified.first(), line.first());
        assert_eq!(simplified.last(), line.last());
    }

    #[test]
    fn rdp_handles_large_input_without_recursing() {
        // Stack-safety smoke test: a zigzag forces a split at (almost)
        // every level, which is the worst case for the explicit stack.
        let n = 20_000;
        let line: Vec<(f64, f64)> = (0..n)
            .map(|i| {
                let x = i as f64;
                let y = if i % 2 == 0 { 0.0 } else { 1.0 };
                (x, y)
            })
            .collect();
        let simplified = simplify_rdp(&line, 0.1);
        assert_eq!(simplified.first(), line.first());
        assert_eq!(simplified.last(), line.last());
        assert!(simplified.len() > 2);
    }

    // ---- clip_to_bounds ----

    const BOUNDS: (f64, f64, f64, f64) = (0.0, 0.0, 10.0, 10.0);

    #[test]
    fn clip_fully_inside_returns_single_unmodified_segment() {
        let line = vec![(1.0, 1.0), (5.0, 5.0), (9.0, 2.0)];
        let clipped = clip_to_bounds(&line, BOUNDS);
        assert_eq!(clipped, vec![line]);
    }

    #[test]
    fn clip_fully_outside_returns_nothing() {
        let line = vec![(-5.0, -5.0), (-1.0, -1.0), (-1.0, 20.0)];
        let clipped = clip_to_bounds(&line, BOUNDS);
        assert!(clipped.is_empty());
    }

    #[test]
    fn clip_single_intersection_inserts_boundary_vertex() {
        // Starts outside (left of the box), ends inside.
        let line = vec![(-5.0, 5.0), (5.0, 5.0)];
        let clipped = clip_to_bounds(&line, BOUNDS);
        assert_eq!(clipped, vec![vec![(0.0, 5.0), (5.0, 5.0)]]);
    }

    #[test]
    fn clip_exit_and_reentry_produces_two_segments() {
        // Inside -> exits right -> re-enters -> inside again.
        let line = vec![(2.0, 5.0), (20.0, 5.0), (20.0, 8.0), (2.0, 8.0)];
        let clipped = clip_to_bounds(&line, BOUNDS);
        assert_eq!(
            clipped,
            vec![vec![(2.0, 5.0), (10.0, 5.0)], vec![(10.0, 8.0), (2.0, 8.0)],]
        );
    }

    #[test]
    fn clip_corner_intersection() {
        // Diagonal line clipped exactly through the box's corners.
        let line = vec![(-5.0, -5.0), (15.0, 15.0)];
        let clipped = clip_to_bounds(&line, BOUNDS);
        assert_eq!(clipped, vec![vec![(0.0, 0.0), (10.0, 10.0)]]);
    }

    #[test]
    fn clip_touching_corner_tangentially_drops_degenerate_point() {
        // Diagonal from upper-left to lower-right of the origin: grazes
        // the (0,0) corner at a single point but is outside the box
        // everywhere else on the segment.
        let line = vec![(-5.0, 5.0), (5.0, -5.0)];
        let clipped = clip_to_bounds(&line, BOUNDS);
        assert!(clipped.is_empty());
    }

    #[test]
    fn clip_short_input_returns_nothing() {
        let empty: Vec<(f64, f64)> = vec![];
        assert!(clip_to_bounds(&empty, BOUNDS).is_empty());

        let one = vec![(5.0, 5.0)];
        assert!(clip_to_bounds(&one, BOUNDS).is_empty());
    }

    // ---- tile grid math ----

    const H: f64 = MERCATOR_HALF_WORLD_M;

    #[test]
    fn tile_math_world_corners_and_center() {
        // z0: the whole world is tile (0,0).
        assert_eq!(mercator_to_tile(0.0, 0.0, 0), (0, 0));
        assert_eq!(tile_bounds(0, 0, 0), (-H, -H, H, H));

        // z1: four quadrants; y is DOWN (north-west is (0,0)).
        assert_eq!(mercator_to_tile(-1.0, 1.0, 1), (0, 0), "NW");
        assert_eq!(mercator_to_tile(1.0, 1.0, 1), (1, 0), "NE");
        assert_eq!(mercator_to_tile(-1.0, -1.0, 1), (0, 1), "SW");
        assert_eq!(mercator_to_tile(1.0, -1.0, 1), (1, 1), "SE");

        // Out-of-world points clamp into edge tiles instead of wrapping.
        assert_eq!(mercator_to_tile(-H - 10.0, H + 10.0, 1), (0, 0));
        assert_eq!(mercator_to_tile(H + 10.0, -H - 10.0, 1), (1, 1));
    }

    #[test]
    fn tile_bounds_roundtrip_through_mercator_to_tile() {
        for &(z, tx, ty) in &[(1u8, 0u32, 1u32), (5, 17, 11), (14, 8710, 5744)] {
            let (min_x, min_y, max_x, max_y) = tile_bounds(z, tx, ty);
            assert!(min_x < max_x && min_y < max_y);
            let cx = (min_x + max_x) / 2.0;
            let cy = (min_y + max_y) / 2.0;
            assert_eq!(mercator_to_tile(cx, cy, z), (tx, ty), "z{z} ({tx},{ty})");
            let extent = tile_extent_m(z);
            assert!((max_x - min_x - extent).abs() < 1e-6);
            assert!((max_y - min_y - extent).abs() < 1e-6);
        }
    }

    #[test]
    fn epsilon_shrinks_with_zoom() {
        // Half a pixel: ~2.4km at z5, ~4.8m at z14 (the Blueprint scaling).
        assert!((epsilon_for_zoom(5) - 2_446.0).abs() < 1.0);
        assert!((epsilon_for_zoom(14) - 4.777).abs() < 0.01);
        assert!(epsilon_for_zoom(14) < epsilon_for_zoom(5) / 100.0);
    }

    #[test]
    fn segment_traversal_covers_straight_and_diagonal_paths() {
        let e = tile_extent_m(14);
        let (tx, ty) = mercator_to_tile(0.5 * e, -0.5 * e, 14); // just SE of origin

        // Within one tile.
        let mut tiles = BTreeSet::new();
        tiles_crossed_by_segment((0.1 * e, -0.5 * e), (0.9 * e, -0.5 * e), 14, &mut tiles);
        assert_eq!(tiles.into_iter().collect::<Vec<_>>(), vec![(tx, ty)]);

        // Horizontal crossing of 3 tiles.
        let mut tiles = BTreeSet::new();
        tiles_crossed_by_segment((0.5 * e, -0.5 * e), (2.5 * e, -0.5 * e), 14, &mut tiles);
        assert_eq!(
            tiles.into_iter().collect::<Vec<_>>(),
            vec![(tx, ty), (tx + 1, ty), (tx + 2, ty)]
        );

        // Diagonal: cost is O(tiles crossed), and endpoints always included.
        let mut tiles = BTreeSet::new();
        tiles_crossed_by_segment((0.5 * e, -0.5 * e), (100.5 * e, -50.5 * e), 14, &mut tiles);
        assert!(tiles.contains(&(tx, ty)), "start tile");
        assert!(tiles.contains(&(tx + 100, ty + 50)), "end tile");
        // A 100x50-tile bbox has 5,151 tiles; the line crosses ~151.
        assert!(
            tiles.len() >= 101 && tiles.len() <= 152,
            "diagonal must visit ~|dx|+|dy| tiles, got {}",
            tiles.len()
        );

        // Degenerate zero-length segment: exactly its own tile.
        let mut tiles = BTreeSet::new();
        tiles_crossed_by_segment((0.5 * e, -0.5 * e), (0.5 * e, -0.5 * e), 14, &mut tiles);
        assert_eq!(tiles.len(), 1);
    }
}
