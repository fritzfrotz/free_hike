//! Pass 3 — tile binning (Phase 4, P4.C2).
//!
//! Walks the [`WAYS`] table sequentially, materializes each way's Web
//! Mercator linestring one at a time (the Blueprint's transient-geometry
//! rule under the 50MB ceiling), simplifies it for the base zoom, clips it
//! to every tile it crosses, and stores the resulting disjoint segments in
//! [`TILE_FEATURES`] — the per-tile feature index the MVT encode stage
//! (Phase 6) will drain.
//!
//! **Suspendability:** the cursor is the last fully-binned way ID
//! (`Checkpoint::pass3_last_way_id` in the engine). Features are flushed to
//! redb before the cursor is ever reported, so a checkpoint never runs
//! ahead of durable data; re-processing a way after a crash overwrites the
//! same `(z, x, y, way_id)` keys with identical values (idempotent upsert)
//! — the same at-worst-zero-duplication contract as Passes 1 and 2.

use std::collections::BTreeSet;

use redb::{Database, ReadableDatabase, TableDefinition, TableError};

use crate::{
    assemble_way_geometry, db_err, push_varint, read_varint, IndexError, DEFAULT_BATCH_SIZE, WAYS,
};

/// Pass-3 tile index: `(zoom, tile_x, tile_y, way_id)` → encoded disjoint
/// clipped segments (see [`encode_tile_segments`]). The way ID lives in the
/// composite KEY, not the value: each way inserts its own fresh rows, so
/// binning never read-modify-writes a shared per-tile blob — writes stay
/// append-shaped and batched. Range scans over `(z, x, y, ..)` recover a
/// tile's full feature list in key order.
pub const TILE_FEATURES: TableDefinition<(u8, u32, u32, u64), &[u8]> =
    TableDefinition::new("TileFeatures");

/// The fixed zoom Pass 3 bins at — the base vector-tile level; overview
/// zooms are derived later from these tiles, not re-binned from raw ways.
pub const BASE_TILE_ZOOM: u8 = 14;

/// Rendering buffer around each tile, as a fraction of the tile's extent:
/// the MVT convention of 64 units on a 4096-unit tile. Geometry is clipped
/// to the buffered box so strokes drawn at tile edges have real geometry to
/// join against in the neighbouring tile (Blueprint: "bounding box plus a
/// minor rendering buffer").
pub const TILE_BUFFER_RATIO: f64 = 64.0 / 4096.0;

/// A tile's clip box: its exact bounds expanded by [`TILE_BUFFER_RATIO`].
pub fn tile_clip_bounds(zoom: u8, tx: u32, ty: u32) -> (f64, f64, f64, f64) {
    let (min_x, min_y, max_x, max_y) = geom::tile_bounds(zoom, tx, ty);
    let b = geom::tile_extent_m(zoom) * TILE_BUFFER_RATIO;
    (min_x - b, min_y - b, max_x + b, max_y + b)
}

// ---------------------------------------------------------------------------
// Segment serialization (the TILE_FEATURES value format)
// ---------------------------------------------------------------------------

/// Encodes clipped disjoint segments as
/// `varint(n_segments) [varint(n_vertices) (x: f64 LE, y: f64 LE)...]...`.
/// Coordinates stay full-precision f64 Web Mercator meters — quantization
/// to the tile-local integer grid belongs to the MVT encode stage, not the
/// index.
pub fn encode_tile_segments(segments: &[Vec<(f64, f64)>]) -> Vec<u8> {
    let n_vertices: usize = segments.iter().map(Vec::len).sum();
    let mut out = Vec::with_capacity(2 + 2 * segments.len() + 16 * n_vertices);
    push_varint(&mut out, segments.len() as u64);
    for seg in segments {
        push_varint(&mut out, seg.len() as u64);
        for &(x, y) in seg {
            out.extend_from_slice(&x.to_le_bytes());
            out.extend_from_slice(&y.to_le_bytes());
        }
    }
    out
}

/// Decodes a [`TILE_FEATURES`] value back into disjoint segments. Truncated
/// or trailing bytes are typed errors — a torn value must never silently
/// decode into wrong geometry.
pub fn decode_tile_segments(bytes: &[u8]) -> Result<Vec<Vec<(f64, f64)>>, IndexError> {
    let corrupt = |what: &str| IndexError::InvalidInput(format!("corrupted tile feature: {what}"));

    let mut pos = 0usize;
    let n_segments = read_varint(bytes, &mut pos)?;
    // Each declared segment costs at least 1 length byte: a count larger
    // than the remaining bytes is corruption, caught before any allocation.
    if n_segments > (bytes.len() - pos) as u64 {
        return Err(corrupt("segment count exceeds payload"));
    }

    let mut segments = Vec::with_capacity(n_segments as usize);
    for _ in 0..n_segments {
        let n_vertices = read_varint(bytes, &mut pos)?;
        let need = n_vertices
            .checked_mul(16)
            .and_then(|n| usize::try_from(n).ok())
            .ok_or_else(|| corrupt("vertex count overflows"))?;
        if bytes.len() - pos < need {
            return Err(corrupt("truncated vertex data"));
        }
        let mut seg = Vec::with_capacity(n_vertices as usize);
        for _ in 0..n_vertices {
            let x = f64::from_le_bytes(bytes[pos..pos + 8].try_into().unwrap());
            let y = f64::from_le_bytes(bytes[pos + 8..pos + 16].try_into().unwrap());
            pos += 16;
            seg.push((x, y));
        }
        segments.push(seg);
    }
    if pos != bytes.len() {
        return Err(corrupt("trailing bytes after last segment"));
    }
    Ok(segments)
}

// ---------------------------------------------------------------------------
// Feature writes and reads
// ---------------------------------------------------------------------------

/// One binned feature awaiting insert: key + pre-encoded segments.
type PendingFeature = ((u8, u32, u32, u64), Vec<u8>);

/// Inserts pending features into [`TILE_FEATURES`] in chunked write
/// transactions — same commit/thread-safety contract as
/// [`crate::insert_coords_batched`]. Returns the number inserted.
pub fn insert_tile_features_batched<I>(
    db: &Database,
    features: I,
    batch_size: usize,
) -> Result<u64, IndexError>
where
    I: IntoIterator<Item = PendingFeature>,
{
    if batch_size == 0 {
        return Err(IndexError::InvalidInput(
            "batch_size must be at least 1".to_string(),
        ));
    }
    let mut features = features.into_iter();
    let mut total: u64 = 0;
    loop {
        let tx = db.begin_write().map_err(db_err)?;
        let mut in_chunk: usize = 0;
        {
            let mut table = tx.open_table(TILE_FEATURES).map_err(db_err)?;
            for (key, value) in features.by_ref().take(batch_size) {
                table.insert(key, value.as_slice()).map_err(db_err)?;
                in_chunk += 1;
            }
        }
        if in_chunk == 0 {
            tx.abort().map_err(db_err)?;
            break;
        }
        tx.commit().map_err(db_err)?;
        total += in_chunk as u64;
        if in_chunk < batch_size {
            break;
        }
    }
    Ok(total)
}

/// One decoded feature read back from a tile: the way ID and its clipped
/// disjoint segments.
pub type TileFeature = (u64, Vec<Vec<(f64, f64)>>);

/// All features binned into tile `(zoom, tx, ty)`, in ascending way order.
/// `Ok(empty)` for an untouched tile or a not-yet-created table.
pub fn get_tile_features(
    db: &Database,
    zoom: u8,
    tile_x: u32,
    tile_y: u32,
) -> Result<Vec<TileFeature>, IndexError> {
    let tx = db.begin_read().map_err(db_err)?;
    let table = match tx.open_table(TILE_FEATURES) {
        Ok(t) => t,
        Err(TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
        Err(e) => return Err(db_err(e)),
    };
    let range = table
        .range((zoom, tile_x, tile_y, 0)..=(zoom, tile_x, tile_y, u64::MAX))
        .map_err(db_err)?;
    let mut out = Vec::new();
    for entry in range {
        let (key, value) = entry.map_err(db_err)?;
        let (_, _, _, way_id) = key.value();
        out.push((way_id, decode_tile_segments(value.value())?));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Pass 3 driver (budget-yieldable)
// ---------------------------------------------------------------------------

/// Result of one Pass-3 slice. Same durable contract as `Pass1Slice`:
/// every feature from ways up to and including `last_way_id` is committed
/// before this struct exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pass3Slice {
    /// Highest way ID fully binned so far (checkpoint this; resume passes it
    /// back). Unchanged from the resume value when the slice binned nothing.
    pub last_way_id: u64,
    /// True when the WAYS table has been exhausted.
    pub finished: bool,
    /// Ways processed by THIS slice (including ways yielding no features).
    pub ways_binned: u32,
    /// Tile features committed by THIS slice.
    pub features_written: u64,
}

/// Bins one simplified way into `sink`: gathers every base-zoom tile any
/// segment crosses (grid traversal — O(tiles crossed), immune to the
/// continent-diagonal bounding-box blowup), dilates by one ring so tiles
/// whose *buffer* the way merely grazes are also considered, then clips to
/// each candidate's buffered box. Tiles where the clip degenerates to
/// nothing produce no row.
fn bin_way(way_id: u64, simplified: &[(f64, f64)], sink: &mut Vec<PendingFeature>) {
    let mut crossed: BTreeSet<(u32, u32)> = BTreeSet::new();
    for pair in simplified.windows(2) {
        geom::tiles_crossed_by_segment(pair[0], pair[1], BASE_TILE_ZOOM, &mut crossed);
    }

    // One-ring dilation: the clip buffer (TILE_BUFFER_RATIO of an extent) is
    // far smaller than a tile, so a way inside a neighbour's buffer zone is
    // always within one ring of a crossed tile.
    let n_minus_1 = (1u32 << BASE_TILE_ZOOM) - 1;
    let mut candidates: BTreeSet<(u32, u32)> = BTreeSet::new();
    for &(tx, ty) in &crossed {
        for dx in -1i64..=1 {
            for dy in -1i64..=1 {
                let cx = i64::from(tx) + dx;
                let cy = i64::from(ty) + dy;
                if (0..=i64::from(n_minus_1)).contains(&cx)
                    && (0..=i64::from(n_minus_1)).contains(&cy)
                {
                    candidates.insert((cx as u32, cy as u32));
                }
            }
        }
    }

    for (tx, ty) in candidates {
        let clipped = geom::clip_to_bounds(simplified, tile_clip_bounds(BASE_TILE_ZOOM, tx, ty));
        if !clipped.is_empty() {
            sink.push((
                (BASE_TILE_ZOOM, tx, ty, way_id),
                encode_tile_segments(&clipped),
            ));
        }
    }
}

/// Runs Pass 3 from the way *after* `resume_after_way_id` (0 = fresh start;
/// OSM way IDs are ≥ 1) until `should_yield` asks for the CPU back (checked
/// after each way, after at least one — the engine's minimum-forward-progress
/// rule) or the WAYS table is exhausted.
///
/// Per way: [`assemble_way_geometry`] materializes the linestring (dropped
/// again before the next way), [`geom::simplify_rdp`] reduces it with the
/// base zoom's epsilon, and [`bin_way`] clips it into every affected tile.
/// Ways whose geometry cannot be assembled (refs outside the extract) are
/// counted as processed and skipped.
pub fn run_pass3_slice(
    db: &Database,
    resume_after_way_id: u64,
    should_yield: &mut dyn FnMut() -> bool,
) -> Result<Pass3Slice, IndexError> {
    let mut last_way_id = resume_after_way_id;
    let mut ways_binned = 0u32;
    let mut features_written = 0u64;
    let mut buffer: Vec<PendingFeature> = Vec::new();

    let tx = db.begin_read().map_err(db_err)?;
    let table = match tx.open_table(WAYS) {
        Ok(t) => t,
        // No ways at all (e.g. an extract with no renderable ways): Pass 3
        // is trivially complete.
        Err(TableError::TableDoesNotExist(_)) => {
            return Ok(Pass3Slice {
                last_way_id,
                finished: true,
                ways_binned: 0,
                features_written: 0,
            });
        }
        Err(e) => return Err(db_err(e)),
    };
    let mut iter = table
        .range(resume_after_way_id.saturating_add(1)..)
        .map_err(db_err)?;

    let epsilon = geom::epsilon_for_zoom(BASE_TILE_ZOOM);
    let finished = loop {
        let Some(entry) = iter.next() else {
            break true;
        };
        let (key, _) = entry.map_err(db_err)?;
        let way_id = key.value();

        // Transient join: the linestring lives exactly as long as this
        // iteration (50MB-ceiling posture).
        if let Some(line) = assemble_way_geometry(db, way_id)? {
            let simplified = geom::simplify_rdp(&line, epsilon);
            bin_way(way_id, &simplified, &mut buffer);
        }
        last_way_id = way_id;
        ways_binned += 1;

        if buffer.len() >= DEFAULT_BATCH_SIZE {
            features_written +=
                insert_tile_features_batched(db, buffer.drain(..), DEFAULT_BATCH_SIZE)?;
        }
        if should_yield() {
            break false;
        }
    };

    // Flush BEFORE reporting the cursor (same durability rule as Pass 1/2).
    features_written += insert_tile_features_batched(db, buffer.drain(..), DEFAULT_BATCH_SIZE)?;

    Ok(Pass3Slice {
        last_way_id,
        finished,
        ways_binned,
        features_written,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{insert_coords_batched, insert_ways_batched, open_coord_db};
    use std::fs;
    use std::path::PathBuf;

    fn tmp_dir(tag: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("freehike-tile-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    // -- serialization -----------------------------------------------------

    #[test]
    fn tile_segments_roundtrip() {
        let cases: Vec<Vec<Vec<(f64, f64)>>> = vec![
            vec![],
            vec![vec![(0.0, 0.0), (1.5, -2.5)]],
            vec![
                vec![(1_268_000.25, 5_988_000.75), (1_268_100.0, 5_988_100.0)],
                vec![(-1.0, -2.0), (-3.0, -4.0), (-5.0, -6.0)],
            ],
        ];
        for segments in cases {
            let encoded = encode_tile_segments(&segments);
            assert_eq!(
                decode_tile_segments(&encoded).unwrap(),
                segments,
                "roundtrip {segments:?}"
            );
        }
    }

    #[test]
    fn tile_segments_reject_garbage() {
        // Truncated mid-vertex.
        let full = encode_tile_segments(&[vec![(1.0, 2.0), (3.0, 4.0)]]);
        assert!(matches!(
            decode_tile_segments(&full[..full.len() - 5]),
            Err(IndexError::InvalidInput(_))
        ));
        // Trailing garbage.
        let mut padded = full.clone();
        padded.push(0xAB);
        assert!(matches!(
            decode_tile_segments(&padded),
            Err(IndexError::InvalidInput(_))
        ));
        // Hostile segment count with no data behind it.
        assert!(matches!(
            decode_tile_segments(&[0xff, 0xff, 0xff, 0x7f]),
            Err(IndexError::InvalidInput(_))
        ));
        // Empty input (not even a count).
        assert!(matches!(
            decode_tile_segments(&[]),
            Err(IndexError::InvalidInput(_))
        ));
    }

    // -- binning ------------------------------------------------------------

    /// Test grid anchor: tile (8192, 8192) at z14 spans merc [0, E] × [-E, 0]
    /// — the tile just south-east of the projection origin, so boundary
    /// coordinates are exact small numbers.
    fn anchor() -> (f64, (u32, u32)) {
        let e = geom::tile_extent_m(BASE_TILE_ZOOM);
        let (tx, ty) = geom::mercator_to_tile(0.5 * e, -0.5 * e, BASE_TILE_ZOOM);
        (e, (tx, ty))
    }

    fn buffer_m() -> f64 {
        geom::tile_extent_m(BASE_TILE_ZOOM) * TILE_BUFFER_RATIO
    }

    /// The required integration proof: a way crossing a tile boundary is
    /// split into BOTH tiles' feature lists, each clipped to its own
    /// buffered box with exact boundary-intersection vertices.
    #[test]
    fn way_crossing_tile_boundary_splits_into_both_tiles() {
        let dir = tmp_dir("split");
        let db = open_coord_db(&dir.join("index.redb")).unwrap();
        let (e, (tx, ty)) = anchor();
        let b = buffer_m();

        // Horizontal way crossing the x = 0 boundary between tile (tx-1, ty)
        // and tile (tx, ty), well clear of any y boundary and of the buffer.
        let a = (-0.4 * e, -0.5 * e);
        let z = (0.4 * e, -0.5 * e);
        insert_coords_batched(&db, [(1u64, a), (2u64, z)], DEFAULT_BATCH_SIZE).unwrap();
        insert_ways_batched(&db, [(77u64, vec![1u64, 2])], DEFAULT_BATCH_SIZE).unwrap();

        let s = run_pass3_slice(&db, 0, &mut || false).unwrap();
        assert!(s.finished);
        assert_eq!(s.ways_binned, 1);
        assert_eq!(s.last_way_id, 77);
        assert_eq!(s.features_written, 2, "exactly the two crossed tiles");

        // Un-clipped endpoints round-trip bit-exact; the inserted boundary
        // vertices are parametrically interpolated, so compare those with a
        // small tolerance.
        let approx = |got: (f64, f64), want: (f64, f64)| {
            assert!(
                (got.0 - want.0).abs() < 1e-6 && (got.1 - want.1).abs() < 1e-6,
                "vertex {got:?} != {want:?}"
            );
        };

        // Left tile: from the way's start to the buffered boundary.
        let left = get_tile_features(&db, BASE_TILE_ZOOM, tx - 1, ty).unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].0, 77);
        assert_eq!(left[0].1.len(), 1, "one contiguous run in the left tile");
        assert_eq!(left[0].1[0].first(), Some(&a), "start survives unclipped");
        approx(*left[0].1[0].last().unwrap(), (b, -0.5 * e));

        // Right tile: from the buffered boundary to the way's end.
        let right = get_tile_features(&db, BASE_TILE_ZOOM, tx, ty).unwrap();
        assert_eq!(right.len(), 1);
        assert_eq!(right[0].1.len(), 1, "one contiguous run in the right tile");
        approx(right[0].1[0][0], (-b, -0.5 * e));
        assert_eq!(right[0].1[0].last(), Some(&z), "end survives unclipped");

        // No leakage into a tile the way never comes near.
        assert!(get_tile_features(&db, BASE_TILE_ZOOM, tx + 5, ty)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn way_inside_one_tile_bins_once_unclipped() {
        let dir = tmp_dir("inside");
        let db = open_coord_db(&dir.join("index.redb")).unwrap();
        let (e, (tx, ty)) = anchor();

        // Zigzag well inside tile (tx, ty), > buffer away from every edge.
        let pts = [
            (0.3 * e, -0.3 * e),
            (0.5 * e, -0.6 * e),
            (0.7 * e, -0.35 * e),
        ];
        insert_coords_batched(
            &db,
            pts.iter().enumerate().map(|(i, &p)| (i as u64 + 1, p)),
            DEFAULT_BATCH_SIZE,
        )
        .unwrap();
        insert_ways_batched(&db, [(9u64, vec![1u64, 2, 3])], DEFAULT_BATCH_SIZE).unwrap();

        let s = run_pass3_slice(&db, 0, &mut || false).unwrap();
        assert_eq!(s.features_written, 1, "single tile, no neighbour leakage");

        let feats = get_tile_features(&db, BASE_TILE_ZOOM, tx, ty).unwrap();
        // The zigzag deviates ~0.2*E (hundreds of meters) — far above the
        // ~4.8m z14 epsilon, so simplification must keep all 3 vertices and
        // the fully-inside clip must return them unmodified.
        assert_eq!(feats, vec![(9, vec![pts.to_vec()])]);
    }

    #[test]
    fn pass3_yields_and_resumes_without_duplicates() {
        let dir = tmp_dir("yield");
        let db = open_coord_db(&dir.join("index.redb")).unwrap();
        let (e, (tx, ty)) = anchor();

        // Three single-tile ways with distinct geometry.
        let mut coords = Vec::new();
        let mut ways = Vec::new();
        for w in 0..3u64 {
            let y = -(0.2 + 0.2 * w as f64) * e;
            coords.push((w * 2 + 1, (0.2 * e, y)));
            coords.push((w * 2 + 2, (0.8 * e, y)));
            ways.push((w + 1, vec![w * 2 + 1, w * 2 + 2]));
        }
        insert_coords_batched(&db, coords, DEFAULT_BATCH_SIZE).unwrap();
        insert_ways_batched(&db, ways, DEFAULT_BATCH_SIZE).unwrap();

        // Worst-case slicing: yield after every way.
        let mut cursor = 0u64;
        let mut total_ways = 0u32;
        let mut total_features = 0u64;
        let mut slices = 0u32;
        loop {
            let s = run_pass3_slice(&db, cursor, &mut || true).unwrap();
            assert!(
                s.ways_binned == 1 || (s.finished && s.ways_binned == 0),
                "slice made no progress: {s:?}"
            );
            total_ways += s.ways_binned;
            total_features += s.features_written;
            cursor = s.last_way_id;
            slices += 1;
            if s.finished {
                break;
            }
            assert!(slices < 100, "did not finish");
        }

        assert_eq!(slices, 4, "3 ways + 1 exhaustion-detecting slice");
        assert_eq!(total_ways, 3, "per-slice sums == distinct ways");
        assert_eq!(total_features, 3, "no re-binning duplication");
        assert_eq!(
            get_tile_features(&db, BASE_TILE_ZOOM, tx, ty)
                .unwrap()
                .len(),
            3
        );
    }

    #[test]
    fn pass3_skips_unassemblable_ways_but_still_progresses() {
        let dir = tmp_dir("skip");
        let db = open_coord_db(&dir.join("index.redb")).unwrap();
        let (e, (tx, ty)) = anchor();

        insert_coords_batched(
            &db,
            [(1u64, (0.3 * e, -0.5 * e)), (2u64, (0.7 * e, -0.5 * e))],
            DEFAULT_BATCH_SIZE,
        )
        .unwrap();
        insert_ways_batched(
            &db,
            [
                (10u64, vec![900u64, 901]), // refs entirely outside the extract
                (11u64, vec![1u64, 2]),     // assemblable
            ],
            DEFAULT_BATCH_SIZE,
        )
        .unwrap();

        let s = run_pass3_slice(&db, 0, &mut || false).unwrap();
        assert!(s.finished);
        assert_eq!(
            s.ways_binned, 2,
            "unassemblable way still counts as progress"
        );
        assert_eq!(s.last_way_id, 11);
        assert_eq!(s.features_written, 1);
        assert_eq!(
            get_tile_features(&db, BASE_TILE_ZOOM, tx, ty)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn pass3_on_empty_or_absent_ways_table_finishes_immediately() {
        let dir = tmp_dir("empty");
        let db = open_coord_db(&dir.join("index.redb")).unwrap();
        let s = run_pass3_slice(&db, 0, &mut || false).unwrap();
        assert!(s.finished);
        assert_eq!(s.ways_binned, 0);
        assert_eq!(s.features_written, 0);
        assert_eq!(s.last_way_id, 0);

        // Resume-at-exhaustion is the legitimate "already finished" call.
        insert_ways_batched(&db, [(5u64, vec![1u64, 2])], DEFAULT_BATCH_SIZE).unwrap();
        let s = run_pass3_slice(&db, 5, &mut || false).unwrap();
        assert!(s.finished);
        assert_eq!(s.ways_binned, 0);
        assert_eq!(s.last_way_id, 5, "cursor unchanged when nothing binned");
    }

    #[test]
    fn pass3_simplifies_before_binning() {
        let dir = tmp_dir("simplify");
        let db = open_coord_db(&dir.join("index.redb")).unwrap();
        let (e, (tx, ty)) = anchor();

        // Collinear chain with sub-epsilon wiggle: 5 vertices in, 2 out.
        let y = -0.5 * e;
        let eps = geom::epsilon_for_zoom(BASE_TILE_ZOOM);
        let coords = [
            (1u64, (0.2 * e, y)),
            (2, (0.35 * e, y + 0.1 * eps)),
            (3, (0.5 * e, y)),
            (4, (0.65 * e, y - 0.1 * eps)),
            (5, (0.8 * e, y)),
        ];
        insert_coords_batched(&db, coords, DEFAULT_BATCH_SIZE).unwrap();
        insert_ways_batched(&db, [(3u64, vec![1u64, 2, 3, 4, 5])], DEFAULT_BATCH_SIZE).unwrap();

        run_pass3_slice(&db, 0, &mut || false).unwrap();
        let feats = get_tile_features(&db, BASE_TILE_ZOOM, tx, ty).unwrap();
        assert_eq!(feats.len(), 1);
        assert_eq!(
            feats[0].1,
            vec![vec![(0.2 * e, y), (0.8 * e, y)]],
            "sub-epsilon vertices must be simplified away before storage"
        );
    }
}
