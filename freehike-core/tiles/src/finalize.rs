//! The Finalize drivers: budget-yieldable MVT encode + PMTiles assembly.
//!
//! ## Durability contract (mirrors Passes 1-3)
//!
//! **Encode** (`run_finalize_encode_slice`) drains `TileFeatures` one tile
//! at a time. Per slice it holds ONE redb write transaction over two
//! bookkeeping tables — [`TILE_ENTRIES`] (PMTiles tile ID → payload
//! `(offset, length)` in the temporary data file) and [`PAYLOAD_HASHES`]
//! (FNV-1a 64 of the gzipped payload → first occurrence, byte-verified on
//! every hit so a hash collision can never alias the wrong tile). Payload
//! bytes are `fsync`'d to the data file **before** that transaction
//! commits, and the engine checkpoints the cursor only after both — the
//! checkpoint never runs ahead of durable data. A crash mid-append leaves
//! a torn tail past the entries' high-water mark, truncated on the next
//! slice; a crash between commit and checkpoint makes the resume re-encode
//! a tile into an identical payload, which dedups back onto the same bytes
//! (idempotent; only the `bytes_written` telemetry drifts, exactly like
//! Pass 1's re-scan overcount).
//!
//! **Assembly** (`assemble_archive`) is one idempotent block: it reads the
//! finished entry set, re-orders payloads into ascending-tile-ID (Hilbert)
//! order — making the archive genuinely `clustered` per spec, with dedup
//! entries pointing back at first occurrences — and writes
//! `header | root directory | metadata | (empty leaf section) | tile data`
//! to a temp file, fsyncs, and renames. Killed after the rename, a resume
//! simply re-assembles byte-identical output.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::ops::Bound;
use std::path::Path;

use redb::{
    Database, ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition, TableError,
};

use pbf::tile::{decode_tile_segments, BASE_TILE_ZOOM, TILE_FEATURES};
use pbf::IndexError;

use crate::hilbert::{tile_id, tile_id_to_zxy};
use crate::mvt::{encode_tile_mvt, LAYER_NAME};
use crate::pmtiles::{encode_header, gzip, serialize_directory, DirEntry, Header, HEADER_BYTES};

/// Finalize bookkeeping: PMTiles tile ID → `(offset, length)` of the
/// gzipped MVT payload in the temporary data file. Lives in the same
/// per-job redb index as the pipeline tables; purged with it.
pub const TILE_ENTRIES: TableDefinition<u64, (u64, u64)> =
    TableDefinition::new("FinalizeTileEntries");

/// Payload dedup: FNV-1a 64 of the gzipped payload → first occurrence
/// `(offset, length)`. Hits are byte-verified against the data file before
/// reuse; on the (astronomically rare, but load-bearing) verified mismatch
/// the payload is appended fresh and the first mapping stands.
const PAYLOAD_HASHES: TableDefinition<u64, (u64, u64)> =
    TableDefinition::new("FinalizePayloadHashes");

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum FinalizeError {
    /// Filesystem failure on the data file or archive.
    Io(String),
    /// redb failure or corrupted index content.
    Index(String),
    /// Internal invariant broken (corrupt cursor, impossible state).
    Corrupt(String),
}

impl std::fmt::Display for FinalizeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FinalizeError::Io(m) => write!(f, "I/O error: {m}"),
            FinalizeError::Index(m) => write!(f, "index error: {m}"),
            FinalizeError::Corrupt(m) => write!(f, "state corrupt: {m}"),
        }
    }
}

impl std::error::Error for FinalizeError {}

impl From<IndexError> for FinalizeError {
    fn from(e: IndexError) -> Self {
        FinalizeError::Index(e.to_string())
    }
}

fn db_err(e: impl std::fmt::Display) -> FinalizeError {
    FinalizeError::Index(e.to_string())
}

fn io_err(e: std::io::Error) -> FinalizeError {
    FinalizeError::Io(e.to_string())
}

// ---------------------------------------------------------------------------
// Encode stage
// ---------------------------------------------------------------------------

/// Result of one encode slice. Same contract as `Pass3Slice`: everything up
/// to and including `last_tile_id` is durable before this struct exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalizeSlice {
    /// PMTiles tile ID of the last fully-encoded tile (checkpoint this;
    /// resume passes it back; 0 = fresh — no real tile at our zooms has
    /// ID 0). Unchanged from the resume value when the slice encoded
    /// nothing.
    pub last_tile_id: u64,
    /// True when every `TileFeatures` row has been drained.
    pub finished: bool,
    /// Tiles processed by THIS slice (including tiles whose geometry
    /// degenerated to no payload).
    pub tiles_encoded: u32,
    /// `TileFeatures` rows consumed by THIS slice (progress numerator).
    pub features_drained: u32,
    /// Payload bytes newly appended to the data file by THIS slice
    /// (dedup hits append nothing).
    pub payload_bytes_written: u64,
}

/// Total rows in `TileFeatures` — the encode stage's stable progress
/// denominator (Pass 3 is complete before Finalize runs). 0 if absent.
pub fn tile_feature_row_count(db: &Database) -> Result<u64, FinalizeError> {
    let tx = db.begin_read().map_err(db_err)?;
    match tx.open_table(TILE_FEATURES) {
        Ok(t) => t.len().map_err(db_err),
        Err(TableError::TableDoesNotExist(_)) => Ok(0),
        Err(e) => Err(db_err(e)),
    }
}

/// Committed high-water mark of the data file: the end of the last payload
/// any committed entry references. Bytes past it are a torn tail from a
/// crash mid-append and must be truncated before appending resumes.
fn entries_high_water(db: &Database) -> Result<u64, FinalizeError> {
    let tx = db.begin_read().map_err(db_err)?;
    let table = match tx.open_table(TILE_ENTRIES) {
        Ok(t) => t,
        Err(TableError::TableDoesNotExist(_)) => return Ok(0),
        Err(e) => return Err(db_err(e)),
    };
    let mut high = 0u64;
    for row in table.iter().map_err(db_err)? {
        let (_, v) = row.map_err(db_err)?;
        let (off, len) = v.value();
        high = high.max(off + len);
    }
    Ok(high)
}

fn read_back(file: &mut File, offset: u64, len: u64) -> Result<Vec<u8>, FinalizeError> {
    file.seek(SeekFrom::Start(offset)).map_err(io_err)?;
    let mut buf = vec![0u8; len as usize];
    file.read_exact(&mut buf).map_err(io_err)?;
    Ok(buf)
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Runs one budget-bounded encode slice over `TileFeatures`, appending
/// gzipped MVT payloads to `data_path` and entries to [`TILE_ENTRIES`].
/// `should_yield` is checked between tiles after at least one tile has been
/// processed (the engine's minimum-forward-progress rule). See the module
/// docs for the crash-consistency argument.
pub fn run_finalize_encode_slice(
    db: &Database,
    data_path: &Path,
    resume_last_tile_id: u64,
    should_yield: &mut dyn FnMut() -> bool,
) -> Result<FinalizeSlice, FinalizeError> {
    let mut slice = FinalizeSlice {
        last_tile_id: resume_last_tile_id,
        finished: false,
        tiles_encoded: 0,
        features_drained: 0,
        payload_bytes_written: 0,
    };

    // Truncate any torn tail past the committed high-water mark before the
    // first new append. A file SHORTER than the high-water mark means
    // committed entries reference bytes that don't exist — hard corruption.
    let high_water = entries_high_water(db)?;
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(data_path)
        .map_err(io_err)?;
    let file_len = file.metadata().map_err(io_err)?.len();
    match file_len.cmp(&high_water) {
        std::cmp::Ordering::Greater => file.set_len(high_water).map_err(io_err)?,
        std::cmp::Ordering::Less => {
            return Err(FinalizeError::Corrupt(format!(
                "tile data file is {file_len} bytes but committed entries reach {high_water}"
            )));
        }
        std::cmp::Ordering::Equal => {}
    }
    let mut end_offset = high_water;

    let read_tx = db.begin_read().map_err(db_err)?;
    let features = match read_tx.open_table(TILE_FEATURES) {
        Ok(t) => t,
        Err(TableError::TableDoesNotExist(_)) => {
            slice.finished = true;
            return Ok(slice);
        }
        Err(e) => return Err(db_err(e)),
    };

    // Resume strictly after every row of the cursor tile.
    let start: Bound<(u8, u32, u32, u64)> = if resume_last_tile_id == 0 {
        Bound::Unbounded
    } else {
        let (z, x, y) = tile_id_to_zxy(resume_last_tile_id).ok_or_else(|| {
            FinalizeError::Corrupt(format!(
                "resume tile id {resume_last_tile_id} is outside the tile-ID space"
            ))
        })?;
        Bound::Excluded((z, x, y, u64::MAX))
    };
    let range = features.range((start, Bound::Unbounded)).map_err(db_err)?;
    let mut rows = range.peekable();

    let write_tx = db.begin_write().map_err(db_err)?;
    let mut inserted_any = false;
    {
        let mut entries_table = write_tx.open_table(TILE_ENTRIES).map_err(db_err)?;
        let mut hashes_table = write_tx.open_table(PAYLOAD_HASHES).map_err(db_err)?;

        loop {
            // ---- Collect exactly one tile's rows -------------------------
            let Some(first) = rows.next() else {
                slice.finished = true;
                break;
            };
            let (key, value) = first.map_err(db_err)?;
            let (z, x, y, first_way) = key.value();
            let mut feats = vec![(first_way, decode_tile_segments(value.value())?)];
            loop {
                let same_tile = match rows.peek() {
                    Some(Ok((pk, _))) => {
                        let (pz, px, py, _) = pk.value();
                        (pz, px, py) == (z, x, y)
                    }
                    // Force consumption below so the error propagates.
                    Some(Err(_)) => true,
                    None => false,
                };
                if !same_tile {
                    break;
                }
                let (k2, v2) = rows.next().expect("peeked").map_err(db_err)?;
                let (_, _, _, way) = k2.value();
                feats.push((way, decode_tile_segments(v2.value())?));
            }

            // ---- Encode, dedup, append -----------------------------------
            let tid = tile_id(z, x, y);
            if let Some(payload) = encode_tile_mvt(z, x, y, &feats) {
                let gz = gzip(&payload);
                let gz_len = gz.len() as u64;
                let hash = fnv1a(&gz);

                let existing = hashes_table
                    .get(hash)
                    .map_err(db_err)?
                    .map(|guard| guard.value());
                // Byte-verify every hash hit before reusing it: a 64-bit
                // collision must degrade to a duplicate payload, never to
                // the wrong tile.
                let verified_hit = match existing {
                    Some((eo, el)) if el == gz_len => read_back(&mut file, eo, el)? == gz,
                    _ => false,
                };
                let (offset, length) = if verified_hit {
                    existing.expect("verified hit implies existing")
                } else {
                    file.seek(SeekFrom::Start(end_offset)).map_err(io_err)?;
                    file.write_all(&gz).map_err(io_err)?;
                    let offset = end_offset;
                    end_offset += gz_len;
                    slice.payload_bytes_written += gz_len;
                    if existing.is_none() {
                        hashes_table
                            .insert(hash, (offset, gz_len))
                            .map_err(db_err)?;
                    }
                    (offset, gz_len)
                };
                entries_table
                    .insert(tid, (offset, length))
                    .map_err(db_err)?;
                inserted_any = true;
            }

            slice.last_tile_id = tid;
            slice.tiles_encoded += 1;
            slice.features_drained += feats.len() as u32;

            if should_yield() {
                slice.finished = rows.peek().is_none();
                break;
            }
        }
    }

    if inserted_any {
        // Payload bytes must be durable BEFORE the entries referencing them.
        file.sync_all().map_err(io_err)?;
        write_tx.commit().map_err(db_err)?;
    } else {
        write_tx.abort().map_err(db_err)?;
    }
    Ok(slice)
}

// ---------------------------------------------------------------------------
// Assembly stage
// ---------------------------------------------------------------------------

/// Stats from a completed assembly, for the engine's accounting and the
/// completion UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchiveInfo {
    pub addressed_tiles: u64,
    pub tile_entries: u64,
    /// Distinct payloads after dedup.
    pub tile_contents: u64,
    /// Bytes in the tile-data section.
    pub tile_data_bytes: u64,
    /// Total archive size (header + directories + metadata + data).
    pub archive_bytes: u64,
}

/// Assembles the final PMTiles v3 archive at `out_path` from the committed
/// [`TILE_ENTRIES`] and the temporary data file. Idempotent: safe to re-run
/// after a crash at any point (atomic temp + rename). `bounds_deg` is the
/// job bbox `(west, south, east, north)`.
pub fn assemble_archive(
    db: &Database,
    data_path: &Path,
    out_path: &Path,
    bounds_deg: (f64, f64, f64, f64),
) -> Result<ArchiveInfo, FinalizeError> {
    // Entry set, already in ascending tile-ID order (redb key order).
    let read_tx = db.begin_read().map_err(db_err)?;
    let entries: Vec<(u64, u64, u64)> = match read_tx.open_table(TILE_ENTRIES) {
        Ok(t) => {
            let mut out = Vec::new();
            for row in t.iter().map_err(db_err)? {
                let (k, v) = row.map_err(db_err)?;
                let (off, len) = v.value();
                out.push((k.value(), off, len));
            }
            out
        }
        Err(TableError::TableDoesNotExist(_)) => Vec::new(),
        Err(e) => return Err(db_err(e)),
    };

    // Pass 1 (no I/O): clustered data layout. Payloads take their position
    // from the FIRST entry referencing them; later dedup entries point back.
    let mut new_offsets: HashMap<(u64, u64), u64> = HashMap::new();
    let mut copy_order: Vec<(u64, u64)> = Vec::new();
    let mut data_len = 0u64;
    let mut dir = Vec::with_capacity(entries.len());
    let (mut min_zoom, mut max_zoom) = (u8::MAX, 0u8);

    for &(tid, old_off, len) in &entries {
        let payload = (old_off, len);
        let offset = match new_offsets.get(&payload) {
            Some(&o) => o,
            None => {
                let o = data_len;
                new_offsets.insert(payload, o);
                copy_order.push(payload);
                data_len += len;
                o
            }
        };
        dir.push(DirEntry {
            tile_id: tid,
            offset,
            length: len as u32,
            run_length: 1,
        });
        let (z, _, _) = tile_id_to_zxy(tid)
            .ok_or_else(|| FinalizeError::Corrupt(format!("entry tile id {tid} out of range")))?;
        min_zoom = min_zoom.min(z);
        max_zoom = max_zoom.max(z);
    }
    if entries.is_empty() {
        min_zoom = BASE_TILE_ZOOM;
        max_zoom = BASE_TILE_ZOOM;
    }

    let root_dir = gzip(&serialize_directory(&dir));
    let metadata = gzip(
        format!(
            r#"{{"name":"freehike-basemap","format":"pbf","vector_layers":[{{"id":"{LAYER_NAME}","fields":{{}},"minzoom":{min_zoom},"maxzoom":{max_zoom}}}]}}"#
        )
        .as_bytes(),
    );

    let root_dir_offset = HEADER_BYTES as u64;
    let metadata_offset = root_dir_offset + root_dir.len() as u64;
    let leaf_dirs_offset = metadata_offset + metadata.len() as u64;
    let tile_data_offset = leaf_dirs_offset; // leaf section present but empty
    let (west, south, east, north) = bounds_deg;

    let header = Header {
        root_dir_offset,
        root_dir_length: root_dir.len() as u64,
        metadata_offset,
        metadata_length: metadata.len() as u64,
        leaf_dirs_offset,
        leaf_dirs_length: 0,
        tile_data_offset,
        tile_data_length: data_len,
        n_addressed_tiles: entries.len() as u64,
        n_tile_entries: entries.len() as u64,
        n_tile_contents: copy_order.len() as u64,
        clustered: true,
        min_zoom,
        max_zoom,
        bounds_deg,
        center_zoom: min_zoom,
        center_deg: ((west + east) / 2.0, (south + north) / 2.0),
    };

    // Atomic write: temp beside the target, fsync, rename.
    let tmp_path = out_path.with_extension("pmtiles.tmp");
    let out_file = File::create(&tmp_path).map_err(io_err)?;
    let mut out = BufWriter::new(out_file);
    out.write_all(&encode_header(&header)).map_err(io_err)?;
    out.write_all(&root_dir).map_err(io_err)?;
    out.write_all(&metadata).map_err(io_err)?;

    if !copy_order.is_empty() {
        let mut data = File::open(data_path).map_err(io_err)?;
        let mut buf = vec![0u8; 64 * 1024];
        for &(old_off, len) in &copy_order {
            data.seek(SeekFrom::Start(old_off)).map_err(io_err)?;
            let mut remaining = len;
            while remaining > 0 {
                let chunk = remaining.min(buf.len() as u64) as usize;
                data.read_exact(&mut buf[..chunk]).map_err(io_err)?;
                out.write_all(&buf[..chunk]).map_err(io_err)?;
                remaining -= chunk as u64;
            }
        }
    }

    let out_file = out.into_inner().map_err(|e| io_err(e.into_error()))?;
    out_file.sync_all().map_err(io_err)?;
    drop(out_file);
    std::fs::rename(&tmp_path, out_path).map_err(io_err)?;

    Ok(ArchiveInfo {
        addressed_tiles: entries.len() as u64,
        tile_entries: entries.len() as u64,
        tile_contents: copy_order.len() as u64,
        tile_data_bytes: data_len,
        archive_bytes: tile_data_offset + data_len,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mvt::{Tile, MVT_EXTENT};
    use crate::pmtiles::{gunzip, parse_directory};
    use pbf::tile::{encode_tile_segments, insert_tile_features_batched};
    use prost::Message;
    use std::path::PathBuf;

    const BBOX: (f64, f64, f64, f64) = (11.15, 47.05, 11.65, 47.45);

    fn tmp_dir(tag: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("freehike-tiles-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A segment at the same TILE-LOCAL position inside any tile — two
    /// tiles seeded with this produce byte-identical MVT payloads (the
    /// dedup case).
    fn local_segment(z: u8, tx: u32, ty: u32, px: &[(f64, f64)]) -> Vec<(f64, f64)> {
        let (min_x, _, max_x, max_y) = geom::tile_bounds(z, tx, ty);
        let unit = (max_x - min_x) / f64::from(MVT_EXTENT);
        px.iter()
            .map(|&(x, y)| (min_x + x * unit, max_y - y * unit))
            .collect()
    }

    /// Seeds the dummy rows the directive's integration test requires:
    /// three z14 tiles; tiles A and B carry way 42 at identical tile-local
    /// geometry (dedup pair), tile C carries way 7 at different geometry.
    fn seed_dummy_rows(db: &Database) -> [(u8, u32, u32); 3] {
        let a = (BASE_TILE_ZOOM, 8703u32, 5747u32);
        let b = (BASE_TILE_ZOOM, 8704u32, 5747u32);
        let c = (BASE_TILE_ZOOM, 8703u32, 5748u32);
        let line = [(100.0, 100.0), (900.0, 400.0), (2000.0, 2000.0)];
        let other = [(50.0, 3000.0), (3000.0, 50.0)];
        let rows = vec![
            (
                (a.0, a.1, a.2, 42u64),
                encode_tile_segments(&[local_segment(a.0, a.1, a.2, &line)]),
            ),
            (
                (b.0, b.1, b.2, 42u64),
                encode_tile_segments(&[local_segment(b.0, b.1, b.2, &line)]),
            ),
            (
                (c.0, c.1, c.2, 7u64),
                encode_tile_segments(&[local_segment(c.0, c.1, c.2, &other)]),
            ),
        ];
        insert_tile_features_batched(db, rows, 100).unwrap();
        [a, b, c]
    }

    struct ParsedHeader {
        root: (u64, u64),
        metadata: (u64, u64),
        leaf: (u64, u64),
        data: (u64, u64),
        addressed: u64,
        entries: u64,
        contents: u64,
        clustered: u8,
        internal_compression: u8,
        tile_compression: u8,
        tile_type: u8,
        min_zoom: u8,
        max_zoom: u8,
        bounds_e7: (i32, i32, i32, i32),
    }

    fn parse_header(bytes: &[u8]) -> ParsedHeader {
        assert_eq!(&bytes[0..7], b"PMTiles", "magic");
        assert_eq!(bytes[7], 3, "spec version");
        let u = |at: usize| u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap());
        let i = |at: usize| i32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        ParsedHeader {
            root: (u(8), u(16)),
            metadata: (u(24), u(32)),
            leaf: (u(40), u(48)),
            data: (u(56), u(64)),
            addressed: u(72),
            entries: u(80),
            contents: u(88),
            clustered: bytes[96],
            internal_compression: bytes[97],
            tile_compression: bytes[98],
            tile_type: bytes[99],
            min_zoom: bytes[100],
            max_zoom: bytes[101],
            bounds_e7: (i(102), i(106), i(110), i(114)),
        }
    }

    fn no_yield() -> impl FnMut() -> bool {
        || false
    }

    /// The directive's required integration test: dummy `TileFeatures`
    /// rows → encode → assemble → binary header sanity + full readback.
    #[test]
    fn archive_from_dummy_rows_validates_header() {
        let dir = tmp_dir("integration");
        let db = pbf::open_coord_db(&dir.join("job.index.redb")).unwrap();
        let tiles = seed_dummy_rows(&db);
        let data_path = dir.join("job.tiledata.tmp");
        let out_path = dir.join("job.pmtiles");

        let slice = run_finalize_encode_slice(&db, &data_path, 0, &mut no_yield()).unwrap();
        assert!(slice.finished);
        assert_eq!(slice.tiles_encoded, 3);
        assert_eq!(slice.features_drained, 3);
        assert!(slice.payload_bytes_written > 0);

        let info = assemble_archive(&db, &data_path, &out_path, BBOX).unwrap();
        let bytes = std::fs::read(&out_path).unwrap();

        // Header field sanity against the spec's fixed offsets.
        let h = parse_header(&bytes);
        assert_eq!(h.root.0, HEADER_BYTES as u64);
        assert_eq!(h.metadata.0, h.root.0 + h.root.1, "sections contiguous");
        assert_eq!(h.leaf.0, h.metadata.0 + h.metadata.1);
        assert_eq!(h.leaf.1, 0, "root-only directory layout");
        assert_eq!(h.data.0, h.leaf.0);
        assert_eq!(
            h.data.0 + h.data.1,
            bytes.len() as u64,
            "data section ends exactly at EOF"
        );
        assert_eq!((h.addressed, h.entries), (3, 3));
        assert_eq!(h.contents, 2, "identical payloads must dedup");
        assert_eq!(h.clustered, 1);
        assert_eq!(h.internal_compression, 2);
        assert_eq!(h.tile_compression, 2);
        assert_eq!(h.tile_type, 1, "MVT");
        assert_eq!((h.min_zoom, h.max_zoom), (BASE_TILE_ZOOM, BASE_TILE_ZOOM));
        assert_eq!(
            h.bounds_e7,
            (111_500_000, 470_500_000, 116_500_000, 474_500_000)
        );
        assert_eq!(info.archive_bytes, bytes.len() as u64);

        // Root directory readback: gunzip → parse → sorted, dedup shares.
        let root_bytes = gunzip(&bytes[h.root.0 as usize..(h.root.0 + h.root.1) as usize]);
        let entries = parse_directory(&root_bytes);
        assert_eq!(entries.len(), 3);
        let expected_ids: Vec<u64> = {
            let mut ids: Vec<u64> = tiles.iter().map(|&(z, x, y)| tile_id(z, x, y)).collect();
            ids.sort_unstable();
            ids
        };
        assert_eq!(
            entries.iter().map(|e| e.tile_id).collect::<Vec<_>>(),
            expected_ids,
            "directory sorted by Hilbert tile ID"
        );

        // Dedup pair (tiles A and B) share one payload.
        let e_by_id = |id: u64| *entries.iter().find(|e| e.tile_id == id).unwrap();
        let ea = e_by_id(tile_id(tiles[0].0, tiles[0].1, tiles[0].2));
        let eb = e_by_id(tile_id(tiles[1].0, tiles[1].1, tiles[1].2));
        assert_eq!((ea.offset, ea.length), (eb.offset, eb.length));

        // Every payload gunzips and prost-decodes to our layer.
        for e in &entries {
            let start = (h.data.0 + e.offset) as usize;
            let payload = gunzip(&bytes[start..start + e.length as usize]);
            let tile = Tile::decode(payload.as_slice()).unwrap();
            assert_eq!(tile.layers.len(), 1);
            assert_eq!(tile.layers[0].name, LAYER_NAME);
            assert_eq!(tile.layers[0].extent, MVT_EXTENT);
            assert!(!tile.layers[0].features.is_empty());
        }

        // Clustered invariant: first-occurrence offsets ascend with tile ID.
        let mut seen_end = 0u64;
        for e in &entries {
            if e.offset >= seen_end {
                assert_eq!(e.offset, seen_end, "first occurrences are contiguous");
                seen_end = e.offset + u64::from(e.length);
            } // else: dedup back-reference, allowed by the clustered spec
        }

        // Metadata section is valid gzip'd JSON naming our layer.
        let meta = gunzip(&bytes[h.metadata.0 as usize..(h.metadata.0 + h.metadata.1) as usize]);
        let meta = String::from_utf8(meta).unwrap();
        assert!(meta.contains(r#""vector_layers""#) && meta.contains(LAYER_NAME));
    }

    #[test]
    fn encode_yields_per_tile_and_resumes_without_duplication() {
        let dir = tmp_dir("yield");
        let db = pbf::open_coord_db(&dir.join("job.index.redb")).unwrap();
        seed_dummy_rows(&db);
        let data_path = dir.join("job.tiledata.tmp");

        let mut cursor = 0u64;
        let mut slices = 0u32;
        let mut total_tiles = 0u32;
        let mut appended = 0u64;
        loop {
            let s = run_finalize_encode_slice(&db, &data_path, cursor, &mut || true).unwrap();
            assert!(s.tiles_encoded <= 1, "yield-every-tile must slice per tile");
            // The cursor is monotonic in (z,x,y) SCAN order — the Hilbert
            // IDs it stores deliberately jump around the row-major scan.
            if s.tiles_encoded > 0 && cursor != 0 {
                let prev = tile_id_to_zxy(cursor).unwrap();
                let cur = tile_id_to_zxy(s.last_tile_id).unwrap();
                assert!(prev < cur, "cursor must advance in scan order");
            }
            cursor = s.last_tile_id;
            slices += 1;
            total_tiles += s.tiles_encoded;
            appended += s.payload_bytes_written;
            if s.finished {
                break;
            }
            assert!(slices < 16, "runaway slice loop");
        }
        assert_eq!(total_tiles, 3);

        // Zero duplication: sliced total equals a fresh single-shot run.
        let dir2 = tmp_dir("yield-single");
        let db2 = pbf::open_coord_db(&dir2.join("job.index.redb")).unwrap();
        seed_dummy_rows(&db2);
        let single =
            run_finalize_encode_slice(&db2, &dir2.join("d.tmp"), 0, &mut no_yield()).unwrap();
        assert_eq!(appended, single.payload_bytes_written);

        // Both archives assemble byte-identically.
        let a1 = dir.join("a.pmtiles");
        let a2 = dir2.join("a.pmtiles");
        assemble_archive(&db, &data_path, &a1, BBOX).unwrap();
        assemble_archive(&db2, &dir2.join("d.tmp"), &a2, BBOX).unwrap();
        assert_eq!(std::fs::read(a1).unwrap(), std::fs::read(a2).unwrap());
    }

    /// The crash path: a stale checkpoint makes the engine re-encode tiles
    /// whose entries already committed. Every payload must dedup back onto
    /// the same bytes — nothing appended, entries unchanged.
    #[test]
    fn reencode_after_stale_cursor_is_idempotent() {
        let dir = tmp_dir("stale");
        let db = pbf::open_coord_db(&dir.join("job.index.redb")).unwrap();
        seed_dummy_rows(&db);
        let data_path = dir.join("job.tiledata.tmp");

        let first = run_finalize_encode_slice(&db, &data_path, 0, &mut no_yield()).unwrap();
        assert!(first.finished && first.payload_bytes_written > 0);
        let len_after_first = std::fs::metadata(&data_path).unwrap().len();

        // Cursor rolled all the way back — the worst stale-checkpoint case.
        let second = run_finalize_encode_slice(&db, &data_path, 0, &mut no_yield()).unwrap();
        assert!(second.finished);
        assert_eq!(second.tiles_encoded, 3, "tiles are re-processed");
        assert_eq!(second.payload_bytes_written, 0, "but nothing re-appended");
        assert_eq!(
            std::fs::metadata(&data_path).unwrap().len(),
            len_after_first
        );
    }

    #[test]
    fn torn_tail_is_truncated_on_resume() {
        let dir = tmp_dir("torn");
        let db = pbf::open_coord_db(&dir.join("job.index.redb")).unwrap();
        seed_dummy_rows(&db);
        let data_path = dir.join("job.tiledata.tmp");

        let s = run_finalize_encode_slice(&db, &data_path, 0, &mut no_yield()).unwrap();
        let clean_len = std::fs::metadata(&data_path).unwrap().len();

        // Simulate a crash mid-append: garbage past the committed payloads.
        let mut f = OpenOptions::new().append(true).open(&data_path).unwrap();
        f.write_all(b"TORN-TAIL-GARBAGE").unwrap();
        drop(f);

        let resumed =
            run_finalize_encode_slice(&db, &data_path, s.last_tile_id, &mut no_yield()).unwrap();
        assert!(resumed.finished);
        assert_eq!(resumed.tiles_encoded, 0);
        assert_eq!(
            std::fs::metadata(&data_path).unwrap().len(),
            clean_len,
            "torn tail must be truncated to the entries' high-water mark"
        );

        // The archive still assembles with fully valid payloads.
        let out = dir.join("job.pmtiles");
        assemble_archive(&db, &data_path, &out, BBOX).unwrap();
        let bytes = std::fs::read(&out).unwrap();
        let h = parse_header(&bytes);
        let root = gunzip(&bytes[h.root.0 as usize..(h.root.0 + h.root.1) as usize]);
        for e in parse_directory(&root) {
            let start = (h.data.0 + e.offset) as usize;
            gunzip(&bytes[start..start + e.length as usize]); // panics if torn
        }
    }

    #[test]
    fn empty_index_produces_valid_empty_archive() {
        let dir = tmp_dir("empty");
        let db = pbf::open_coord_db(&dir.join("job.index.redb")).unwrap();
        let data_path = dir.join("job.tiledata.tmp");

        assert_eq!(tile_feature_row_count(&db).unwrap(), 0);
        let s = run_finalize_encode_slice(&db, &data_path, 0, &mut no_yield()).unwrap();
        assert!(s.finished);
        assert_eq!((s.tiles_encoded, s.last_tile_id), (0, 0));

        let out = dir.join("job.pmtiles");
        let info = assemble_archive(&db, &data_path, &out, BBOX).unwrap();
        assert_eq!(info.tile_entries, 0);
        assert_eq!(info.tile_data_bytes, 0);

        let bytes = std::fs::read(&out).unwrap();
        let h = parse_header(&bytes);
        assert_eq!(h.data.0 + h.data.1, bytes.len() as u64);
        assert_eq!((h.entries, h.contents), (0, 0));
        let root = gunzip(&bytes[h.root.0 as usize..(h.root.0 + h.root.1) as usize]);
        assert!(parse_directory(&root).is_empty());
    }

    #[test]
    fn tile_feature_row_count_counts_rows() {
        let dir = tmp_dir("rowcount");
        let db = pbf::open_coord_db(&dir.join("job.index.redb")).unwrap();
        assert_eq!(tile_feature_row_count(&db).unwrap(), 0);
        seed_dummy_rows(&db);
        assert_eq!(tile_feature_row_count(&db).unwrap(), 3);
    }
}
