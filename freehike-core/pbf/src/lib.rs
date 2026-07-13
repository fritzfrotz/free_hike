//! `pbf` — out-of-core OSM node indexing (Phase 3).
//!
//! The pipeline must compile country-scale `.osm.pbf` extracts (multi-GB) on a
//! phone under a **50MB RAM ceiling**. Two mechanisms enforce it:
//!
//! 1. **Zero-copy input** ([`PbfMmap`]): the raw PBF is memory-mapped
//!    read-only, never read into a heap buffer. Mapped pages are file-backed
//!    and *clean* (we never write through the map), so the OS can evict them
//!    under pressure at zero cost — they are cache, not heap, and do not
//!    count against the ceiling.
//!
//! 2. **Disk-backed index with a bounded cache** ([`open_coord_db`]): Pass 1
//!    stores node-ID → Web-Mercator coordinates in a redb database whose page
//!    cache is capped at [`REDB_CACHE_BYTES`]. redb's cache is the only
//!    unbounded-by-default heap consumer in the pipeline; capping it is what
//!    makes the index *out-of-core* rather than merely on-disk.
//!
//! Writes go through [`insert_coords_batched`]: one write transaction per
//! chunk of [`DEFAULT_BATCH_SIZE`] nodes. redb commits are durable (fsync), so
//! per-row transactions would thrash the disk; chunking amortizes one fsync
//! across thousands of rows while keeping the uncommitted working set small
//! and the job resumable at chunk granularity (same yield philosophy as
//! `compiler::engine`).
//!
//! Decoding lives in [`proto`] (hand-derived prost messages for the frozen
//! OSM PBF wire format) and [`scan`] (block scanner + the suspendable Pass-1
//! driver [`scan::run_pass1_slice`]).

pub mod proto;
pub mod scan;

#[cfg(any(test, feature = "fixtures"))]
pub mod fixtures;

pub use scan::{
    run_pass1_slice, stringtable_has_relevant_keys, BlockKind, BlockScanner, Pass1Slice,
    RELEVANT_TAG_KEYS,
};

use std::fmt;
use std::fs::File;
use std::path::Path;

use memmap2::Mmap;
use redb::{Database, ReadableDatabase, ReadableTableMetadata, TableDefinition, TableError};

// ---------------------------------------------------------------------------
// RAM budget
// ---------------------------------------------------------------------------

/// The project-wide hard ceiling for compile-pipeline heap use.
pub const RAM_CEILING_BYTES: usize = 50 * 1024 * 1024;

/// redb page-cache cap. Deliberately *less* than [`RAM_CEILING_BYTES`]: the
/// ceiling is a whole-process budget, and the remainder is reserved for the
/// PBF decode buffers, the in-flight write batch, and shell/FFI overhead.
/// Setting the cache to the full ceiling would guarantee a breach the moment
/// anything else allocates.
pub const REDB_CACHE_BYTES: usize = 32 * 1024 * 1024;

// Budget invariants, enforced at compile time: the cache must sit under the
// ceiling with at least 8MB of headroom for everything that isn't the cache.
const _: () = assert!(REDB_CACHE_BYTES < RAM_CEILING_BYTES);
const _: () = assert!(RAM_CEILING_BYTES - REDB_CACHE_BYTES >= 8 * 1024 * 1024);

/// Nodes per write transaction in [`insert_coords_batched`]. One durable
/// commit (fsync) per this many rows.
pub const DEFAULT_BATCH_SIZE: usize = 10_000;

/// Pass-1 node index: OSM node ID → Web Mercator `(x, y)` in meters.
pub const COORDINATES: TableDefinition<u64, (f64, f64)> = TableDefinition::new("Coordinates");

/// Pass-2 way index: OSM way ID → compact byte encoding of the way's ordered
/// node-ID sequence (see [`encode_way_refs`]). Values are opaque byte arrays,
/// NOT in-memory coordinate vectors — under the 50MB ceiling a way's geometry
/// is only ever materialized transiently, one way at a time, by joining these
/// refs against [`COORDINATES`] at encode time.
pub const WAYS: TableDefinition<u64, &[u8]> = TableDefinition::new("Ways");

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Indexing failures. Plain enum (no `thiserror`), matching the error style of
/// the `compiler` and `fetcher` crates, so the FFI layer can flatten it cheaply.
#[derive(Debug)]
pub enum IndexError {
    /// Filesystem error opening or mapping the input.
    Io(String),
    /// redb storage/transaction error.
    Db(String),
    /// Caller misuse (empty input file, zero batch size, ...).
    InvalidInput(String),
}

impl fmt::Display for IndexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IndexError::Io(s) => write!(f, "io error: {s}"),
            IndexError::Db(s) => write!(f, "db error: {s}"),
            IndexError::InvalidInput(s) => write!(f, "invalid input: {s}"),
        }
    }
}

impl std::error::Error for IndexError {}

fn db_err(e: impl fmt::Display) -> IndexError {
    IndexError::Db(e.to_string())
}

// ---------------------------------------------------------------------------
// Memory-mapped PBF reader
// ---------------------------------------------------------------------------

/// Read-only, zero-copy view of a `.osm.pbf` file on disk.
///
/// # Invariant
/// The mapped file must not be truncated or rewritten while this struct is
/// alive (a shrink would make later page accesses fault). The pipeline
/// guarantees this: `fetcher` downloads to a scratch path and the file is
/// immutable once magic-byte validation passes; compilation only ever reads it.
#[derive(Debug)]
pub struct PbfMmap {
    mmap: Mmap,
}

impl PbfMmap {
    /// Maps `path` read-only. Rejects empty files up front (zero-length maps
    /// are platform-dependent errors, and an empty PBF is invalid anyway).
    pub fn open(path: &Path) -> Result<Self, IndexError> {
        let file = File::open(path)
            .map_err(|e| IndexError::Io(format!("open {}: {e}", path.display())))?;
        let len = file
            .metadata()
            .map_err(|e| IndexError::Io(format!("stat {}: {e}", path.display())))?
            .len();
        if len == 0 {
            return Err(IndexError::InvalidInput(format!(
                "{} is empty — not a PBF",
                path.display()
            )));
        }
        // SAFETY: the map is read-only and the file-immutability invariant is
        // documented above; no aliasing writes exist in this process.
        let mmap = unsafe { Mmap::map(&file) }
            .map_err(|e| IndexError::Io(format!("mmap {}: {e}", path.display())))?;
        Ok(Self { mmap })
    }

    /// The whole file as a byte slice. Zero-copy: this borrows the mapping.
    pub fn bytes(&self) -> &[u8] {
        &self.mmap
    }

    pub fn len(&self) -> usize {
        self.mmap.len()
    }

    pub fn is_empty(&self) -> bool {
        // `open` rejects empty files, but keep the pair honest for clippy and
        // future construction paths.
        self.mmap.is_empty()
    }

    /// Bounds-checked window, for walking `[BlobHeader][Blob]` framing without
    /// arithmetic overflow on hostile length fields. `None` = out of range.
    pub fn slice(&self, offset: usize, len: usize) -> Option<&[u8]> {
        let end = offset.checked_add(len)?;
        self.mmap.get(offset..end)
    }
}

impl AsRef<[u8]> for PbfMmap {
    fn as_ref(&self) -> &[u8] {
        self.bytes()
    }
}

// ---------------------------------------------------------------------------
// Coordinate index (redb)
// ---------------------------------------------------------------------------

/// Creates (or reopens) the coordinate index at `path` with the page cache
/// capped at [`REDB_CACHE_BYTES`] — the enforcement point of the RAM ceiling.
pub fn open_coord_db(path: &Path) -> Result<Database, IndexError> {
    Database::builder()
        .set_cache_size(REDB_CACHE_BYTES)
        .create(path)
        .map_err(|e| IndexError::Db(format!("open index {}: {e}", path.display())))
}

/// Inserts `(node_id, (merc_x, merc_y))` pairs in chunked write transactions
/// of `batch_size` rows, committing (fsync) once per chunk.
///
/// Thread-safety: takes `&Database` (which is `Sync`); redb serializes write
/// transactions internally, so concurrent callers interleave at chunk
/// granularity instead of corrupting each other. Duplicate node IDs follow
/// last-write-wins semantics (an OSM extract never legitimately repeats one).
///
/// Returns the number of rows inserted. On error, all fully-committed chunks
/// survive (durable); only the in-flight chunk is rolled back — same
/// resume-at-a-boundary contract as the compile engine's checkpoints.
pub fn insert_coords_batched<I>(
    db: &Database,
    nodes: I,
    batch_size: usize,
) -> Result<u64, IndexError>
where
    I: IntoIterator<Item = (u64, (f64, f64))>,
{
    if batch_size == 0 {
        return Err(IndexError::InvalidInput(
            "batch_size must be at least 1".to_string(),
        ));
    }

    let mut nodes = nodes.into_iter();
    let mut total: u64 = 0;
    loop {
        let tx = db.begin_write().map_err(db_err)?;
        let mut in_chunk: usize = 0;
        {
            let mut table = tx.open_table(COORDINATES).map_err(db_err)?;
            for (id, xy) in nodes.by_ref().take(batch_size) {
                table.insert(id, xy).map_err(db_err)?;
                in_chunk += 1;
            }
        }
        if in_chunk == 0 {
            // Iterator exhausted exactly at a chunk boundary — nothing to
            // commit. (abort() also ensures a fresh-empty-iterator call
            // doesn't create the table as a side effect... except it does
            // open it above; abort discards that too.)
            tx.abort().map_err(db_err)?;
            break;
        }
        tx.commit().map_err(db_err)?;
        total += in_chunk as u64;
        if in_chunk < batch_size {
            break; // final short chunk — iterator exhausted
        }
    }
    Ok(total)
}

/// Point lookup. `Ok(None)` for an absent node *or* a not-yet-created table
/// (Pass 2 may probe before Pass 1 has written anything).
pub fn get_coord(db: &Database, node_id: u64) -> Result<Option<(f64, f64)>, IndexError> {
    let tx = db.begin_read().map_err(db_err)?;
    let table = match tx.open_table(COORDINATES) {
        Ok(t) => t,
        Err(TableError::TableDoesNotExist(_)) => return Ok(None),
        Err(e) => return Err(db_err(e)),
    };
    Ok(table.get(node_id).map_err(db_err)?.map(|g| g.value()))
}

/// Total indexed nodes (0 if the table hasn't been created yet).
pub fn coord_count(db: &Database) -> Result<u64, IndexError> {
    let tx = db.begin_read().map_err(db_err)?;
    let table = match tx.open_table(COORDINATES) {
        Ok(t) => t,
        Err(TableError::TableDoesNotExist(_)) => return Ok(0),
        Err(e) => return Err(db_err(e)),
    };
    table.len().map_err(db_err)
}

// ---------------------------------------------------------------------------
// Way index (Pass 2 schema): compact node-ref sequences
// ---------------------------------------------------------------------------
//
// A way's node refs are stored as delta + zigzag + LEB128-varint bytes — the
// same integer coding the PBF wire format itself uses for way refs, so the
// value stays a few bytes per node instead of 8, and no in-memory coordinate
// vectors ever exist on the write path (50MB-ceiling posture).

fn push_varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn read_varint(bytes: &[u8], pos: &mut usize) -> Result<u64, IndexError> {
    let mut value: u64 = 0;
    for shift in (0..64).step_by(7) {
        let &byte = bytes.get(*pos).ok_or_else(|| {
            IndexError::InvalidInput("corrupted way refs: truncated varint".to_string())
        })?;
        *pos += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err(IndexError::InvalidInput(
        "corrupted way refs: varint exceeds 64 bits".to_string(),
    ))
}

fn zigzag_encode(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

fn zigzag_decode(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

/// Encodes an ordered node-ID sequence into the compact [`WAYS`] value form.
/// IDs above `i64::MAX` are rejected (OSM IDs are positive sint64 on the
/// wire, so a larger value can only be corruption).
pub fn encode_way_refs(node_ids: &[u64]) -> Result<Vec<u8>, IndexError> {
    let mut out = Vec::with_capacity(node_ids.len() * 2 + 4);
    let mut prev: i64 = 0;
    for &id in node_ids {
        let id = i64::try_from(id).map_err(|_| {
            IndexError::InvalidInput(format!("node id {id} exceeds the OSM sint64 domain"))
        })?;
        push_varint(&mut out, zigzag_encode(id - prev));
        prev = id;
    }
    Ok(out)
}

/// Decodes a [`WAYS`] value back into the ordered node-ID sequence.
pub fn decode_way_refs(bytes: &[u8]) -> Result<Vec<u64>, IndexError> {
    let mut ids = Vec::new();
    let mut pos = 0usize;
    let mut prev: i64 = 0;
    while pos < bytes.len() {
        let delta = zigzag_decode(read_varint(bytes, &mut pos)?);
        prev = prev.checked_add(delta).ok_or_else(|| {
            IndexError::InvalidInput("corrupted way refs: delta overflow".to_string())
        })?;
        let id = u64::try_from(prev).map_err(|_| {
            IndexError::InvalidInput(format!("corrupted way refs: negative node id {prev}"))
        })?;
        ids.push(id);
    }
    Ok(ids)
}

/// Inserts `(way_id, ordered node refs)` pairs into [`WAYS`] in chunked write
/// transactions — same commit/thread-safety contract as
/// [`insert_coords_batched`]. Returns the number of ways inserted.
pub fn insert_ways_batched<I>(db: &Database, ways: I, batch_size: usize) -> Result<u64, IndexError>
where
    I: IntoIterator<Item = (u64, Vec<u64>)>,
{
    if batch_size == 0 {
        return Err(IndexError::InvalidInput(
            "batch_size must be at least 1".to_string(),
        ));
    }
    let mut ways = ways.into_iter();
    let mut total: u64 = 0;
    loop {
        let tx = db.begin_write().map_err(db_err)?;
        let mut in_chunk: usize = 0;
        {
            let mut table = tx.open_table(WAYS).map_err(db_err)?;
            for (way_id, refs) in ways.by_ref().take(batch_size) {
                let encoded = encode_way_refs(&refs)?;
                table.insert(way_id, encoded.as_slice()).map_err(db_err)?;
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

/// Point lookup of a way's ordered node refs. `Ok(None)` for an absent way
/// or a not-yet-created table.
pub fn get_way_refs(db: &Database, way_id: u64) -> Result<Option<Vec<u64>>, IndexError> {
    let tx = db.begin_read().map_err(db_err)?;
    let table = match tx.open_table(WAYS) {
        Ok(t) => t,
        Err(TableError::TableDoesNotExist(_)) => return Ok(None),
        Err(e) => return Err(db_err(e)),
    };
    match table.get(way_id).map_err(db_err)? {
        Some(guard) => Ok(Some(decode_way_refs(guard.value())?)),
        None => Ok(None),
    }
}

// ---------------------------------------------------------------------------
// Projection
// ---------------------------------------------------------------------------

/// Web Mercator latitude limit: beyond this the projection diverges.
pub const MERCATOR_MAX_LAT_DEG: f64 = 85.051_128_779_806_59;

/// WGS84 lon/lat (degrees) → Web Mercator (EPSG:3857) meters. Latitude is
/// clamped to ±[`MERCATOR_MAX_LAT_DEG`] so polar garbage in an extract can
/// never inject ±inf into the index.
pub fn web_mercator(lon_deg: f64, lat_deg: f64) -> (f64, f64) {
    const EARTH_RADIUS_M: f64 = 6_378_137.0;
    let lat = lat_deg.clamp(-MERCATOR_MAX_LAT_DEG, MERCATOR_MAX_LAT_DEG);
    let x = EARTH_RADIUS_M * lon_deg.to_radians();
    let y = EARTH_RADIUS_M
        * (std::f64::consts::FRAC_PI_4 + lat.to_radians() / 2.0)
            .tan()
            .ln();
    (x, y)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn tmp_dir(tag: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("freehike-pbf-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// Deterministic fake coordinate for a node ID, so any test can verify a
    /// read against the ID alone.
    fn coord_for(id: u64) -> (f64, f64) {
        (id as f64 * 1.5, -(id as f64) * 0.25)
    }

    // -- PbfMmap ------------------------------------------------------------

    #[test]
    fn mmap_is_zero_copy_view_of_file() {
        let dir = tmp_dir("mmap");
        let path = dir.join("input.osm.pbf");
        let payload: Vec<u8> = (0..=255u8).cycle().take(70_000).collect();
        fs::write(&path, &payload).unwrap();

        let m = PbfMmap::open(&path).unwrap();
        assert_eq!(m.len(), payload.len());
        assert!(!m.is_empty());
        assert_eq!(m.bytes(), &payload[..], "mapped view must equal file bytes");
        assert_eq!(m.as_ref()[69_999], payload[69_999]);
    }

    #[test]
    fn mmap_slice_is_bounds_checked() {
        let dir = tmp_dir("mmap-slice");
        let path = dir.join("input.osm.pbf");
        fs::write(&path, [1u8, 2, 3, 4, 5, 6, 7, 8]).unwrap();

        let m = PbfMmap::open(&path).unwrap();
        assert_eq!(m.slice(2, 3), Some(&[3u8, 4, 5][..]));
        assert_eq!(m.slice(6, 2), Some(&[7u8, 8][..]));
        assert_eq!(m.slice(6, 3), None, "past EOF");
        assert_eq!(m.slice(usize::MAX, 2), None, "offset overflow");
        assert_eq!(m.slice(0, usize::MAX), None, "hostile length field");
    }

    #[test]
    fn mmap_empty_file_rejected() {
        let dir = tmp_dir("mmap-empty");
        let path = dir.join("empty.osm.pbf");
        fs::write(&path, []).unwrap();
        match PbfMmap::open(&path) {
            Err(IndexError::InvalidInput(msg)) => assert!(msg.contains("empty"), "got: {msg}"),
            other => panic!("expected InvalidInput, got {other:?}"),
        }
    }

    #[test]
    fn mmap_missing_file_is_io_error() {
        match PbfMmap::open(Path::new("/nonexistent/nope.osm.pbf")) {
            Err(IndexError::Io(_)) => {}
            other => panic!("expected Io, got {other:?}"),
        }
    }

    // -- Coordinate index ---------------------------------------------------

    #[test]
    fn db_create_batch_insert_and_read_back() {
        let dir = tmp_dir("db-roundtrip");
        let db = open_coord_db(&dir.join("coords.redb")).unwrap();

        // 25,000 nodes at the default 10,000 chunk = 2 full commits + 1 short.
        let n: u64 = 25_000;
        let total = insert_coords_batched(
            &db,
            (0..n).map(|id| (id, coord_for(id))),
            DEFAULT_BATCH_SIZE,
        )
        .unwrap();
        assert_eq!(total, n);
        assert_eq!(coord_count(&db).unwrap(), n);

        // Spot-check across chunk boundaries: first, straddle, last, absent.
        for id in [0, 1, 9_999, 10_000, 19_999, 20_000, 24_999] {
            assert_eq!(get_coord(&db, id).unwrap(), Some(coord_for(id)), "id {id}");
        }
        assert_eq!(
            get_coord(&db, n).unwrap(),
            None,
            "unindexed id must be None"
        );
    }

    #[test]
    fn short_final_chunk_and_exact_multiple_both_complete() {
        let dir = tmp_dir("db-chunks");
        // 25 rows / batch 10 → 10 + 10 + 5.
        let db = open_coord_db(&dir.join("a.redb")).unwrap();
        let total = insert_coords_batched(&db, (0..25).map(|id| (id, coord_for(id))), 10).unwrap();
        assert_eq!(total, 25);
        assert_eq!(coord_count(&db).unwrap(), 25);

        // 20 rows / batch 10 → exact multiple; the trailing empty transaction
        // must abort cleanly, not commit a phantom chunk.
        let db2 = open_coord_db(&dir.join("b.redb")).unwrap();
        let total = insert_coords_batched(&db2, (0..20).map(|id| (id, coord_for(id))), 10).unwrap();
        assert_eq!(total, 20);
        assert_eq!(coord_count(&db2).unwrap(), 20);
    }

    #[test]
    fn empty_iterator_is_a_noop() {
        let dir = tmp_dir("db-empty");
        let db = open_coord_db(&dir.join("coords.redb")).unwrap();
        let total = insert_coords_batched(&db, std::iter::empty(), DEFAULT_BATCH_SIZE).unwrap();
        assert_eq!(total, 0);
        assert_eq!(coord_count(&db).unwrap(), 0, "no table side effects");
        assert_eq!(get_coord(&db, 42).unwrap(), None);
    }

    #[test]
    fn zero_batch_size_rejected() {
        let dir = tmp_dir("db-zerobatch");
        let db = open_coord_db(&dir.join("coords.redb")).unwrap();
        match insert_coords_batched(&db, [(1u64, (0.0, 0.0))], 0) {
            Err(IndexError::InvalidInput(_)) => {}
            other => panic!("expected InvalidInput, got {other:?}"),
        }
    }

    #[test]
    fn index_survives_reopen() {
        let dir = tmp_dir("db-reopen");
        let path = dir.join("coords.redb");
        {
            let db = open_coord_db(&path).unwrap();
            insert_coords_batched(&db, (0..1_000).map(|id| (id, coord_for(id))), 128).unwrap();
        } // dropped — all handles closed

        let db = open_coord_db(&path).unwrap();
        assert_eq!(coord_count(&db).unwrap(), 1_000);
        assert_eq!(get_coord(&db, 777).unwrap(), Some(coord_for(777)));
    }

    #[test]
    fn concurrent_batched_inserts_from_two_threads() {
        let dir = tmp_dir("db-threads");
        let db = open_coord_db(&dir.join("coords.redb")).unwrap();

        // Disjoint ID ranges through the same &Database; redb serializes the
        // write transactions, callers just interleave at chunk boundaries.
        std::thread::scope(|s| {
            for range in [0..10_000u64, 10_000..20_000u64] {
                let db = &db;
                s.spawn(move || {
                    let n = insert_coords_batched(
                        db,
                        range.clone().map(|id| (id, coord_for(id))),
                        3_000,
                    )
                    .unwrap();
                    assert_eq!(n, 10_000);
                });
            }
        });

        assert_eq!(coord_count(&db).unwrap(), 20_000);
        for id in [0, 9_999, 10_000, 19_999] {
            assert_eq!(get_coord(&db, id).unwrap(), Some(coord_for(id)), "id {id}");
        }
    }

    // -- Way index -------------------------------------------------------

    #[test]
    fn way_refs_roundtrip() {
        let cases: &[&[u64]] = &[
            &[],
            &[42],
            &[100, 101, 102, 103],        // ascending (typical)
            &[5_000, 4_990, 5_010, 1],    // non-monotonic (negative deltas)
            &[0, u64::MAX / 3, 7],        // large jumps both ways
            &[9_223_372_036_854_775_807], // i64::MAX boundary
        ];
        for refs in cases {
            let encoded = encode_way_refs(refs).unwrap();
            assert_eq!(&decode_way_refs(&encoded).unwrap(), refs, "refs {refs:?}");
        }

        // Compactness: consecutive IDs cost one byte per node after the first
        // delta, regardless of the IDs' magnitude.
        let dense: Vec<u64> = (8_000_000_000..8_000_001_000).collect();
        let encoded = encode_way_refs(&dense).unwrap();
        assert!(
            encoded.len() < 1_000 + 8,
            "delta coding must beat 8 bytes/node: {} bytes for 1000 refs",
            encoded.len()
        );
    }

    #[test]
    fn way_refs_reject_garbage() {
        // ID outside the OSM sint64 domain.
        assert!(matches!(
            encode_way_refs(&[u64::MAX]),
            Err(IndexError::InvalidInput(_))
        ));
        // Truncated varint (continuation bit set, then EOF).
        assert!(matches!(
            decode_way_refs(&[0x80]),
            Err(IndexError::InvalidInput(_))
        ));
        // Varint longer than 64 bits.
        assert!(matches!(
            decode_way_refs(&[0xff; 11]),
            Err(IndexError::InvalidInput(_))
        ));
    }

    #[test]
    fn ways_table_batched_insert_and_lookup() {
        let dir = tmp_dir("ways");
        let db = open_coord_db(&dir.join("index.redb")).unwrap();

        // Both tables coexist in the one per-job index database.
        insert_coords_batched(&db, [(7u64, (1.0, 2.0))], DEFAULT_BATCH_SIZE).unwrap();

        let ways: Vec<(u64, Vec<u64>)> = (0..25u64)
            .map(|w| (w, vec![w * 10, w * 10 + 1, w * 10 + 2]))
            .collect();
        let n = insert_ways_batched(&db, ways, 10).unwrap(); // 10 + 10 + 5
        assert_eq!(n, 25);

        assert_eq!(
            get_way_refs(&db, 24).unwrap(),
            Some(vec![240, 241, 242]),
            "last chunk must be committed"
        );
        assert_eq!(get_way_refs(&db, 99).unwrap(), None);
        assert_eq!(get_coord(&db, 7).unwrap(), Some((1.0, 2.0)));

        // Fresh DB: Ways absent entirely → None, not an error.
        let db2 = open_coord_db(&dir.join("empty.redb")).unwrap();
        assert_eq!(get_way_refs(&db2, 1).unwrap(), None);
    }

    // -- Projection ----------------------------------------------------------

    #[test]
    fn web_mercator_known_values() {
        let (x, y) = web_mercator(0.0, 0.0);
        assert!(x.abs() < 1e-9 && y.abs() < 1e-9);

        // Antimeridian: x = R * π.
        let (x, _) = web_mercator(180.0, 0.0);
        assert!((x - 20_037_508.342_789_244).abs() < 1e-6, "x = {x}");

        // Innsbruck (11.39, 47.27) — sanity band, not exactness.
        let (x, y) = web_mercator(11.39, 47.27);
        assert!((1_267_000.0..1_269_000.0).contains(&x), "x = {x}");
        assert!((5_986_000.0..5_990_000.0).contains(&y), "y = {y}");

        // Polar garbage must clamp, never go infinite.
        let (_, y) = web_mercator(0.0, 90.0);
        assert!(y.is_finite());
        assert_eq!(y, web_mercator(0.0, MERCATOR_MAX_LAT_DEG).1);
    }
}
