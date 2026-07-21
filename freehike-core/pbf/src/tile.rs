// SPDX-License-Identifier: Apache-2.0
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
    assemble_way_geometry, db_err, get_way_tags, layer_name, push_varint, read_varint, IndexError,
    DEFAULT_BATCH_SIZE, WAYS,
};

/// Pass-3 tile index: `(zoom, tile_x, tile_y, way_id)` → encoded disjoint
/// clipped segments (see [`encode_tile_segments`]). The way ID lives in the
/// composite KEY, not the value: each way inserts its own fresh rows, so
/// binning never read-modify-writes a shared per-tile blob — writes stay
/// append-shaped and batched. Range scans over `(z, x, y, ..)` recover a
/// tile's full feature list in key order.
pub const TILE_FEATURES: TableDefinition<(u8, u32, u32, u64), &[u8]> =
    TableDefinition::new("TileFeatures");

/// High-bit marker on the key's feature-id slot for NODE-derived features
/// (peaks): OSM node and way id spaces overlap numerically, so without it
/// a peak node could collide with a way binned into the same tile. Real
/// OSM ids stay far below 2^63.
pub const POI_FEATURE_ID_BIT: u64 = 1 << 63;

/// Drains the sparse [`crate::POIS`] table into point rows of
/// [`TILE_FEATURES`] (P-CORE.C8, closes D002): each peak becomes a
/// single-vertex feature in the `natural` layer with `class=peak`, keyed
/// by its z14 tile and `node_id | POI_FEATURE_ID_BIT`. Idempotent
/// (last-write-wins upserts) — safe to re-run on any Pass-3 resume.
/// Returns the number of POIs binned.
pub fn run_poi_binning(db: &Database) -> Result<u64, IndexError> {
    let pois = crate::all_pois(db)?;
    if pois.is_empty() {
        return Ok(0);
    }
    let rows: Vec<PendingFeature> = pois
        .into_iter()
        .map(|(node_id, x, y, name)| {
            let (tx, ty) = geom::mercator_to_tile(x, y, BASE_TILE_ZOOM);
            (
                (BASE_TILE_ZOOM, tx, ty, node_id | POI_FEATURE_ID_BIT),
                encode_tile_segments(2 /* natural */, b"peak", b"", &name, &[vec![(x, y)]]),
            )
        })
        .collect();
    let n = rows.len() as u64;
    insert_tile_features_batched(db, rows, crate::DEFAULT_BATCH_SIZE)?;
    Ok(n)
}

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

/// Encodes a feature's tag metadata + clipped disjoint segments (format v4,
/// P5.C4) as
/// `u8 layer | varint(class_len) class-bytes |
///  varint(sac_len) sac_scale-bytes | varint(name_len) name-bytes |
///  varint(n_segments) [varint(n_vertices) (x: f64 LE, y: f64 LE)...]...`.
/// The tag metadata is denormalized from `WayTags` into every row (bytes on
/// disk, not RAM) so the Finalize drain stays a single-table scan; empty
/// sac_scale/name slots mean "absent" (same sentinel as `WayTags`).
/// Coordinates stay full-precision f64 Web Mercator meters — quantization
/// to the tile-local integer grid belongs to the MVT encode stage, not the
/// index.
pub fn encode_tile_segments(
    layer: u8,
    class: &[u8],
    sac_scale: &[u8],
    name: &[u8],
    segments: &[Vec<(f64, f64)>],
) -> Vec<u8> {
    let n_vertices: usize = segments.iter().map(Vec::len).sum();
    let mut out = Vec::with_capacity(
        8 + class.len() + sac_scale.len() + name.len() + 2 * segments.len() + 16 * n_vertices,
    );
    out.push(layer);
    push_varint(&mut out, class.len() as u64);
    out.extend_from_slice(class);
    push_varint(&mut out, sac_scale.len() as u64);
    out.extend_from_slice(sac_scale);
    push_varint(&mut out, name.len() as u64);
    out.extend_from_slice(name);
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

/// Decoded [`TILE_FEATURES`] value: `(layer, class, sac_scale, name,
/// disjoint segments)` — sac_scale/name empty = absent.
pub type DecodedTileSegments = (u8, Vec<u8>, Vec<u8>, Vec<u8>, Vec<Vec<(f64, f64)>>);

/// Decodes a [`TILE_FEATURES`] value back into tag metadata + disjoint
/// segments. Truncated or trailing bytes, an out-of-range layer index, and
/// hostile lengths are typed errors — a torn value must never silently
/// decode into wrong geometry.
pub fn decode_tile_segments(bytes: &[u8]) -> Result<DecodedTileSegments, IndexError> {
    let corrupt = |what: &str| IndexError::InvalidInput(format!("corrupted tile feature: {what}"));

    let mut pos = 0usize;
    let &layer = bytes.first().ok_or_else(|| corrupt("empty value"))?;
    pos += 1;
    if layer_name(layer).is_none() {
        return Err(corrupt("layer index out of range"));
    }
    // Two length-prefixed string slots: class, then sac_scale.
    let read_slot = |what: &'static str, pos: &mut usize| -> Result<Vec<u8>, IndexError> {
        let len = read_varint(bytes, pos)?;
        let len = usize::try_from(len)
            .ok()
            .filter(|n| bytes.len() - *pos >= *n)
            .ok_or_else(|| corrupt(&format!("{what} length exceeds payload")))?;
        let slot = bytes[*pos..*pos + len].to_vec();
        *pos += len;
        Ok(slot)
    };
    let class = read_slot("class", &mut pos)?;
    let sac_scale = read_slot("sac_scale", &mut pos)?;
    let name = read_slot("name", &mut pos)?;

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
    Ok((layer, class, sac_scale, name, segments))
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

/// One decoded feature read back from a tile: identity, its rendering
/// layer/class (see [`crate::LAYER_KEYS`]), and its clipped disjoint
/// segments.
#[derive(Debug, Clone, PartialEq)]
pub struct TileFeature {
    pub way_id: u64,
    /// Index into [`crate::LAYER_KEYS`] — resolve via [`crate::layer_name`].
    pub layer: u8,
    /// Raw class bytes (the layer tag's value, validated UTF-8).
    pub class: Vec<u8>,
    /// Trail difficulty grade (highway features only; empty = absent).
    pub sac_scale: Vec<u8>,
    /// Label text — the OSM `name` tag, any layer (empty = absent).
    pub name: Vec<u8>,
    pub segments: Vec<Vec<(f64, f64)>>,
}

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
        let (layer, class, sac_scale, name, segments) = decode_tile_segments(value.value())?;
        out.push(TileFeature {
            way_id,
            layer,
            class,
            sac_scale,
            name,
            segments,
        });
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
/// nothing produce no row. The way's `(layer, class, sac_scale, name)` tag
/// metadata is denormalized into every row written (format v4).
#[allow(clippy::too_many_arguments)] // mirrors the v4 slot order verbatim
fn bin_way(
    way_id: u64,
    layer: u8,
    class: &[u8],
    sac_scale: &[u8],
    name: &[u8],
    simplified: &[(f64, f64)],
    sink: &mut Vec<PendingFeature>,
) {
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
                encode_tile_segments(layer, class, sac_scale, name, &clipped),
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
            // Pass 2 writes WAYS and WAY_TAGS in one transaction, so a refs
            // row without a tags row means the index predates P5.C2 —
            // refuse rather than emit un-layered geometry (same posture as
            // an unsupported checkpoint version).
            let (layer, class, sac_scale, name) = get_way_tags(db, way_id)?.ok_or_else(|| {
                IndexError::InvalidInput(format!(
                    "way {way_id} has refs but no tag record — index written by an \
                     incompatible pipeline version"
                ))
            })?;
            let simplified = geom::simplify_rdp(&line, epsilon);
            bin_way(
                way_id,
                layer,
                &class,
                &sac_scale,
                &name,
                &simplified,
                &mut buffer,
            );
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
    use crate::{insert_coords_batched, insert_ways_batched, open_coord_db, IndexedWay};
    use std::fs;
    use std::path::PathBuf;

    fn tmp_dir(tag: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("freehike-tile-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// A default-tagged (`highway=path`, no grade/name) way for binning
    /// tests that don't exercise tag variety themselves.
    fn hw(id: u64, refs: Vec<u64>) -> IndexedWay {
        IndexedWay {
            id,
            layer: 0,
            class: b"path".to_vec(),
            sac_scale: Vec::new(),
            name: Vec::new(),
            refs,
        }
    }

    // -- serialization -----------------------------------------------------

    #[test]
    fn tile_segments_roundtrip() {
        type Case = (
            u8,
            &'static [u8],
            &'static [u8],
            &'static [u8],
            Vec<Vec<(f64, f64)>>,
        );
        let cases: Vec<Case> = vec![
            (0, b"path", b"", b"", vec![]),
            (
                0,
                b"path",
                b"demanding_mountain_hiking",
                // Real-world UTF-8: umlauts + sharp s must survive the
                // byte-slice slot verbatim.
                "Höttinger Höhenstraße".as_bytes(),
                vec![vec![(0.0, 0.0), (1.5, -2.5)]],
            ),
            (
                1,
                b"river",
                b"",
                b"Inn",
                vec![vec![(0.0, 0.0), (1.5, -2.5)]],
            ),
            // All-empty optional slots survive.
            (3, b"", b"", b"", vec![vec![(0.0, 0.0), (1.0, 1.0)]]),
            (
                2,
                b"wood",
                b"",
                b"",
                vec![
                    vec![(1_268_000.25, 5_988_000.75), (1_268_100.0, 5_988_100.0)],
                    vec![(-1.0, -2.0), (-3.0, -4.0), (-5.0, -6.0)],
                ],
            ),
        ];
        for (layer, class, sac, name, segments) in cases {
            let encoded = encode_tile_segments(layer, class, sac, name, &segments);
            assert_eq!(
                decode_tile_segments(&encoded).unwrap(),
                (
                    layer,
                    class.to_vec(),
                    sac.to_vec(),
                    name.to_vec(),
                    segments.clone()
                ),
                "roundtrip layer={layer} class={class:?} sac={sac:?} name={name:?}"
            );
        }
    }

    #[test]
    fn tile_segments_reject_garbage() {
        // Truncated mid-vertex.
        let full = encode_tile_segments(
            0,
            b"path",
            b"hiking",
            b"Goetheweg",
            &[vec![(1.0, 2.0), (3.0, 4.0)]],
        );
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
        // Layer index outside LAYER_KEYS.
        let mut bad_layer = full.clone();
        bad_layer[0] = 9;
        assert!(matches!(
            decode_tile_segments(&bad_layer),
            Err(IndexError::InvalidInput(_))
        ));
        // Hostile class length with no bytes behind it.
        assert!(matches!(
            decode_tile_segments(&[0, 0xff, 0xff, 0xff, 0x7f]),
            Err(IndexError::InvalidInput(_))
        ));
        // Hostile sac_scale length with no bytes behind it (valid class).
        assert!(matches!(
            decode_tile_segments(&[0, 0, 0xff, 0xff, 0xff, 0x7f]),
            Err(IndexError::InvalidInput(_))
        ));
        // Hostile name length with no bytes behind it (valid class+sac).
        assert!(matches!(
            decode_tile_segments(&[0, 0, 0, 0xff, 0xff, 0xff, 0x7f]),
            Err(IndexError::InvalidInput(_))
        ));
        // Value truncated inside the name slot itself.
        let mut short = encode_tile_segments(0, b"p", b"", b"Goetheweg", &[]);
        short.truncate(6); // layer + class slot + empty sac + partial name
        assert!(matches!(
            decode_tile_segments(&short),
            Err(IndexError::InvalidInput(_))
        ));
        // Hostile segment count with no data behind it (all slots valid).
        assert!(matches!(
            decode_tile_segments(&[0, 0, 0, 0, 0xff, 0xff, 0xff, 0x7f]),
            Err(IndexError::InvalidInput(_))
        ));
        // Empty input (not even a layer byte).
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
        // Fully-attributed trail: sac_scale AND name must survive the clip
        // into BOTH tiles alongside layer/class (P5.C3/P5.C4).
        let way = IndexedWay {
            sac_scale: b"alpine_hiking".to_vec(),
            name: "Goetheweg".as_bytes().to_vec(),
            ..hw(77, vec![1, 2])
        };
        insert_ways_batched(&db, [way], DEFAULT_BATCH_SIZE).unwrap();

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

        // Left tile: from the way's start to the buffered boundary. The tag
        // metadata must survive the clip into BOTH tiles (P5.C2).
        let left = get_tile_features(&db, BASE_TILE_ZOOM, tx - 1, ty).unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].way_id, 77);
        assert_eq!((left[0].layer, left[0].class.as_slice()), (0, &b"path"[..]));
        assert_eq!(left[0].sac_scale, b"alpine_hiking".to_vec());
        assert_eq!(left[0].name, "Goetheweg".as_bytes().to_vec());
        assert_eq!(
            left[0].segments.len(),
            1,
            "one contiguous run in the left tile"
        );
        assert_eq!(
            left[0].segments[0].first(),
            Some(&a),
            "start survives unclipped"
        );
        approx(*left[0].segments[0].last().unwrap(), (b, -0.5 * e));

        // Right tile: from the buffered boundary to the way's end.
        let right = get_tile_features(&db, BASE_TILE_ZOOM, tx, ty).unwrap();
        assert_eq!(right.len(), 1);
        assert_eq!(
            (right[0].layer, right[0].class.as_slice()),
            (0, &b"path"[..])
        );
        assert_eq!(right[0].sac_scale, b"alpine_hiking".to_vec());
        assert_eq!(right[0].name, "Goetheweg".as_bytes().to_vec());
        assert_eq!(
            right[0].segments.len(),
            1,
            "one contiguous run in the right tile"
        );
        approx(right[0].segments[0][0], (-b, -0.5 * e));
        assert_eq!(
            right[0].segments[0].last(),
            Some(&z),
            "end survives unclipped"
        );

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
        insert_ways_batched(&db, [hw(9, vec![1, 2, 3])], DEFAULT_BATCH_SIZE).unwrap();

        let s = run_pass3_slice(&db, 0, &mut || false).unwrap();
        assert_eq!(s.features_written, 1, "single tile, no neighbour leakage");

        let feats = get_tile_features(&db, BASE_TILE_ZOOM, tx, ty).unwrap();
        // The zigzag deviates ~0.2*E (hundreds of meters) — far above the
        // ~4.8m z14 epsilon, so simplification must keep all 3 vertices and
        // the fully-inside clip must return them unmodified.
        assert_eq!(
            feats,
            vec![TileFeature {
                way_id: 9,
                layer: 0,
                class: b"path".to_vec(),
                sac_scale: Vec::new(),
                name: Vec::new(),
                segments: vec![pts.to_vec()],
            }]
        );
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
            ways.push(hw(w + 1, vec![w * 2 + 1, w * 2 + 2]));
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
                hw(10, vec![900, 901]), // refs entirely outside the extract
                hw(11, vec![1, 2]),     // assemblable
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
        insert_ways_batched(&db, [hw(5, vec![1, 2])], DEFAULT_BATCH_SIZE).unwrap();
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
        insert_ways_batched(&db, [hw(3, vec![1, 2, 3, 4, 5])], DEFAULT_BATCH_SIZE).unwrap();

        run_pass3_slice(&db, 0, &mut || false).unwrap();
        let feats = get_tile_features(&db, BASE_TILE_ZOOM, tx, ty).unwrap();
        assert_eq!(feats.len(), 1);
        assert_eq!(
            feats[0].segments,
            vec![vec![(0.2 * e, y), (0.8 * e, y)]],
            "sub-epsilon vertices must be simplified away before storage"
        );
    }

    /// P5.C2 posture check: a WAYS row without its WAY_TAGS twin (an index
    /// written by the pre-tag pipeline) must fail loudly, not emit
    /// un-layered geometry.
    #[test]
    fn pass3_rejects_way_without_tag_record() {
        let dir = tmp_dir("no-tags");
        let db = open_coord_db(&dir.join("index.redb")).unwrap();
        let (e, _) = anchor();

        insert_coords_batched(
            &db,
            [(1u64, (0.3 * e, -0.5 * e)), (2u64, (0.7 * e, -0.5 * e))],
            DEFAULT_BATCH_SIZE,
        )
        .unwrap();
        // Simulate a legacy index: refs row present, tags row missing.
        let tx = db.begin_write().unwrap();
        {
            let mut table = tx.open_table(WAYS).unwrap();
            let encoded = crate::encode_way_refs(&[1, 2]).unwrap();
            table.insert(42u64, encoded.as_slice()).unwrap();
        }
        tx.commit().unwrap();

        let err = run_pass3_slice(&db, 0, &mut || false).unwrap_err();
        assert!(
            matches!(&err, IndexError::InvalidInput(m) if m.contains("no tag record")),
            "got: {err:?}"
        );
    }

    #[test]
    fn poi_binning_creates_point_features_with_id_bit() {
        let dir = tmp_dir("poi-bin");
        let db = crate::open_coord_db(&dir.join("idx.redb")).unwrap();
        let merc = crate::web_mercator(11.3908, 47.2757);
        crate::insert_pois(&db, &[(4_242, merc, b"Patscherkofel".to_vec())]).unwrap();

        assert_eq!(run_poi_binning(&db).unwrap(), 1);
        let (tx, ty) = geom::mercator_to_tile(merc.0, merc.1, BASE_TILE_ZOOM);
        let feats = get_tile_features(&db, BASE_TILE_ZOOM, tx, ty).unwrap();
        assert_eq!(feats.len(), 1);
        let f = &feats[0];
        assert_eq!(
            f.way_id,
            4_242 | POI_FEATURE_ID_BIT,
            "node ids carry the POI bit"
        );
        assert_eq!(f.layer, 2, "peaks live in the natural layer");
        assert_eq!(f.class, b"peak".to_vec());
        assert_eq!(f.name, b"Patscherkofel".to_vec());
        assert_eq!(f.segments, vec![vec![merc]], "single-vertex point geometry");

        // Idempotent re-run (Pass-3 resume path): still exactly one row.
        assert_eq!(run_poi_binning(&db).unwrap(), 1);
        assert_eq!(
            get_tile_features(&db, BASE_TILE_ZOOM, tx, ty)
                .unwrap()
                .len(),
            1
        );
    }
}
