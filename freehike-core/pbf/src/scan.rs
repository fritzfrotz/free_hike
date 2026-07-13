//! Block scanner + Pass 1 (node extraction) over a memory-mapped PBF.
//!
//! Framing (per `fileformat.proto`): the file is a sequence of
//! `[u32 BE length][BlobHeader][Blob]` records. The scanner walks that
//! framing with every length field bounds-checked against the map — a
//! hostile or truncated file produces a typed error, never a panic or an
//! out-of-bounds read.
//!
//! **Suspendability:** the scanner's cursor is a plain absolute byte offset,
//! valid only at block boundaries. [`run_pass1_slice`] returns that offset
//! after flushing every extracted node into redb, so the offset a caller
//! checkpoints never runs ahead of durable data. Re-entering with the same
//! offset re-processes at worst zero blocks (inserts are last-write-wins
//! upserts, so even a crash between commit and checkpoint is harmless).
//! This is the same yield contract as `compiler::engine::run_slice`, which
//! will drive this function from its Pass1Nodes phase.

use prost::Message;

use crate::proto::{Blob, BlobHeader, PrimitiveBlock};
use crate::{insert_coords_batched, web_mercator, IndexError, PbfMmap, DEFAULT_BATCH_SIZE};

/// Spec: the serialized BlobHeader "must be less than 64 KiB".
pub const MAX_BLOBHEADER_BYTES: usize = 64 * 1024;

/// Cap on both the serialized Blob and its decompressed payload. The spec
/// SHOULD-limit is 16MiB (MUST < 32MiB); we enforce the SHOULD because the
/// decompression buffer is the largest transient heap allocation in Pass 1
/// and must fit inside the headroom left beside [`crate::REDB_CACHE_BYTES`]
/// under the 50MB ceiling. Real-world extracts (Geofabrik, planet) stay
/// well under it.
pub const MAX_BLOB_PAYLOAD_BYTES: usize = 16 * 1024 * 1024;

fn corrupt(offset: usize, what: impl std::fmt::Display) -> IndexError {
    IndexError::InvalidInput(format!("corrupted PBF at byte {offset}: {what}"))
}

// ---------------------------------------------------------------------------
// Block scanner
// ---------------------------------------------------------------------------

/// One decoded framing record.
#[derive(Debug)]
pub struct ScannedBlock {
    /// Offset of this block's length prefix (its resume point).
    pub start_offset: usize,
    /// Offset of the next block — the resume point after consuming this one.
    pub end_offset: usize,
    pub kind: BlockKind,
}

#[derive(Debug)]
pub enum BlockKind {
    /// "OSMHeader" — file metadata; payload not decoded by Pass 1.
    Header,
    /// "OSMData" — decompressed and decoded.
    Data(PrimitiveBlock),
    /// Unknown blob type — spec requires skipping, not failing.
    Skipped(String),
}

/// Cursor over the `[length][BlobHeader][Blob]` framing of a mapped PBF.
pub struct BlockScanner<'a> {
    data: &'a [u8],
    offset: usize,
}

impl<'a> BlockScanner<'a> {
    pub fn new(pbf: &'a PbfMmap) -> Self {
        Self::resume(pbf, 0)
    }

    /// Re-enters at a previously reported block-boundary offset.
    pub fn resume(pbf: &'a PbfMmap, offset: usize) -> Self {
        Self {
            data: pbf.bytes(),
            offset,
        }
    }

    /// Current absolute byte offset. Valid as a resume point only when the
    /// last `next_block` call returned `Ok` (boundaries only).
    pub fn offset(&self) -> usize {
        self.offset
    }

    /// Decodes the next block and advances. `Ok(None)` = clean end of file.
    pub fn next_block(&mut self) -> Result<Option<ScannedBlock>, IndexError> {
        let start = self.offset;
        if start == self.data.len() {
            return Ok(None);
        }
        if start > self.data.len() {
            return Err(corrupt(start, "resume offset beyond end of file"));
        }

        let len_bytes: [u8; 4] = self
            .data
            .get(start..start + 4)
            .ok_or_else(|| corrupt(start, "truncated BlobHeader length prefix"))?
            .try_into()
            .unwrap();
        let header_len = u32::from_be_bytes(len_bytes) as usize;
        if header_len == 0 || header_len > MAX_BLOBHEADER_BYTES {
            return Err(corrupt(
                start,
                format!("implausible BlobHeader length {header_len}"),
            ));
        }

        let hdr_start = start + 4;
        let hdr_bytes = self
            .data
            .get(hdr_start..hdr_start + header_len)
            .ok_or_else(|| corrupt(hdr_start, "truncated BlobHeader"))?;
        let header = BlobHeader::decode(hdr_bytes)
            .map_err(|e| corrupt(hdr_start, format!("BlobHeader decode: {e}")))?;

        let datasize = usize::try_from(header.datasize)
            .map_err(|_| corrupt(hdr_start, format!("negative datasize {}", header.datasize)))?;
        if datasize == 0 || datasize > MAX_BLOB_PAYLOAD_BYTES {
            return Err(corrupt(
                hdr_start,
                format!("blob datasize {datasize} outside (0, {MAX_BLOB_PAYLOAD_BYTES}]"),
            ));
        }

        let blob_start = hdr_start + header_len;
        let blob_bytes = self
            .data
            .get(blob_start..blob_start + datasize)
            .ok_or_else(|| corrupt(blob_start, "truncated Blob payload"))?;
        let end_offset = blob_start + datasize;

        let kind = match header.r#type.as_str() {
            "OSMHeader" => BlockKind::Header,
            "OSMData" => BlockKind::Data(decode_primitive_block(blob_bytes, blob_start)?),
            other => BlockKind::Skipped(other.to_string()),
        };

        self.offset = end_offset;
        Ok(Some(ScannedBlock {
            start_offset: start,
            end_offset,
            kind,
        }))
    }
}

/// Blob → (decompress) → PrimitiveBlock. The decompression output size is
/// capped by the declared `raw_size` (itself capped), so a zlib bomb cannot
/// blow the RAM budget.
fn decode_primitive_block(blob_bytes: &[u8], offset: usize) -> Result<PrimitiveBlock, IndexError> {
    let blob =
        Blob::decode(blob_bytes).map_err(|e| corrupt(offset, format!("Blob decode: {e}")))?;

    let payload: Vec<u8> = if let Some(raw) = blob.raw {
        raw
    } else if let Some(zlib) = blob.zlib_data {
        let declared = blob
            .raw_size
            .ok_or_else(|| corrupt(offset, "compressed blob missing raw_size"))?;
        let limit = usize::try_from(declared)
            .ok()
            .filter(|&n| n > 0 && n <= MAX_BLOB_PAYLOAD_BYTES)
            .ok_or_else(|| corrupt(offset, format!("implausible raw_size {declared}")))?;
        let out = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(&zlib, limit)
            .map_err(|e| corrupt(offset, format!("zlib inflate: {e}")))?;
        if out.len() != limit {
            return Err(corrupt(
                offset,
                format!("raw_size {limit} != inflated size {}", out.len()),
            ));
        }
        out
    } else {
        return Err(corrupt(
            offset,
            "unsupported blob encoding (only raw and zlib_data are supported)",
        ));
    };

    PrimitiveBlock::decode(payload.as_slice())
        .map_err(|e| corrupt(offset, format!("PrimitiveBlock decode: {e}")))
}

// ---------------------------------------------------------------------------
// StringTable semantic pre-filter (Blueprint: "Semantic Filtering")
// ---------------------------------------------------------------------------

/// Tag keys that make a block semantically relevant to the hiking pipeline,
/// per the Blueprint ("sac_scale for trail difficulty, highway for paths,
/// waterway for streams, or contour-related tags") and the architecture plan
/// (research/, Phase 3/4).
pub const RELEVANT_TAG_KEYS: &[&[u8]] =
    &[b"highway", b"sac_scale", b"waterway", b"natural", b"ele"];

/// True when `block`'s StringTable contains at least one hiking-relevant tag
/// key — the Blueprint's pre-filter for skipping a block "without incurring
/// the CPU cost of deserializing the individual entities".
///
/// **Scope: this gate is for Pass 2 (ways/relations) only.** It must NOT gate
/// Pass-1 node indexing: way vertices are overwhelmingly *untagged* dense
/// nodes, so node-bearing blocks rarely contain `highway`/`sac_scale` in
/// their StringTable — filtering node blocks on tag relevance would hollow
/// out the coordinate index and break Pass-2 geometry reconstruction. Pass 1
/// gets the intended CPU saving structurally instead: the prost messages in
/// [`crate::proto`] leave ways/relations/tags undeclared, so they are skipped
/// at the wire level without being deserialized.
pub fn stringtable_has_relevant_keys(block: &PrimitiveBlock) -> bool {
    block.stringtable.as_ref().is_some_and(|st| {
        st.s.iter()
            .any(|s| RELEVANT_TAG_KEYS.contains(&s.as_slice()))
    })
}

// ---------------------------------------------------------------------------
// Node extraction (delta decoding + projection)
// ---------------------------------------------------------------------------

/// Decodes every node in `block` — DenseNodes (delta-coded parallel arrays)
/// and plain nodes — into `(id, web_mercator(x, y))` pairs appended to `out`.
///
/// Deliberately unfiltered by [`stringtable_has_relevant_keys`] — see that
/// function's docs for why the semantic pre-filter must not gate Pass 1.
pub fn extract_node_coords(
    block: &PrimitiveBlock,
    out: &mut Vec<(u64, (f64, f64))>,
) -> Result<u64, IndexError> {
    let granularity = i64::from(block.granularity.unwrap_or(100));
    let lat_offset = block.lat_offset.unwrap_or(0);
    let lon_offset = block.lon_offset.unwrap_or(0);
    // Spec: degrees = 1e-9 * (offset + granularity * units).
    let project = |lon_units: i64, lat_units: i64| {
        web_mercator(
            1e-9 * (lon_offset + granularity * lon_units) as f64,
            1e-9 * (lat_offset + granularity * lat_units) as f64,
        )
    };

    let mut extracted = 0u64;
    for group in &block.primitivegroup {
        if let Some(dense) = &group.dense {
            let n = dense.id.len();
            if dense.lat.len() != n || dense.lon.len() != n {
                return Err(IndexError::InvalidInput(format!(
                    "corrupted DenseNodes: parallel arrays disagree (id={n}, lat={}, lon={})",
                    dense.lat.len(),
                    dense.lon.len()
                )));
            }
            let (mut id, mut lat, mut lon) = (0i64, 0i64, 0i64);
            for i in 0..n {
                // Delta decoding: each element is the sint64 difference from
                // its predecessor (first is relative to 0).
                id += dense.id[i];
                lat += dense.lat[i];
                lon += dense.lon[i];
                let key = u64::try_from(id).map_err(|_| {
                    IndexError::InvalidInput(format!("corrupted DenseNodes: negative node id {id}"))
                })?;
                out.push((key, project(lon, lat)));
            }
            extracted += n as u64;
        }
        for node in &group.nodes {
            let key = u64::try_from(node.id).map_err(|_| {
                IndexError::InvalidInput(format!("negative plain node id {}", node.id))
            })?;
            out.push((key, project(node.lon, node.lat)));
            extracted += 1;
        }
    }
    Ok(extracted)
}

// ---------------------------------------------------------------------------
// Pass 1 driver (budget-yieldable)
// ---------------------------------------------------------------------------

/// Result of one Pass-1 slice. `next_offset` is durable: every node from
/// blocks before it has been committed to redb before this struct exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pass1Slice {
    /// Block-boundary byte offset to resume from (checkpoint this).
    pub next_offset: usize,
    /// True when the whole file has been scanned; `next_offset == file len`.
    pub finished: bool,
    /// Nodes committed by THIS slice.
    pub nodes_indexed: u64,
    /// Blocks consumed by this slice (including header/skipped blocks).
    pub blocks_scanned: u32,
}

/// Runs Pass 1 from `resume_offset` until `should_yield` asks for the CPU
/// back (checked at block boundaries, after at least one block — the same
/// minimum-forward-progress rule as the compile engine) or the file ends.
///
/// Extracted nodes are buffered and fed to [`insert_coords_batched`] whenever
/// [`DEFAULT_BATCH_SIZE`] is reached, with a final flush before returning, so
/// the reported `next_offset` is always safe to persist as a checkpoint.
pub fn run_pass1_slice(
    pbf: &PbfMmap,
    db: &redb::Database,
    resume_offset: usize,
    should_yield: &mut dyn FnMut() -> bool,
) -> Result<Pass1Slice, IndexError> {
    let mut scanner = BlockScanner::resume(pbf, resume_offset);
    let mut buffer: Vec<(u64, (f64, f64))> = Vec::new();
    let mut nodes_indexed = 0u64;
    let mut blocks_scanned = 0u32;

    let finished = loop {
        match scanner.next_block()? {
            None => break true,
            Some(block) => {
                if let BlockKind::Data(pb) = &block.kind {
                    extract_node_coords(pb, &mut buffer)?;
                }
                blocks_scanned += 1;
                if buffer.len() >= DEFAULT_BATCH_SIZE {
                    nodes_indexed +=
                        insert_coords_batched(db, buffer.drain(..), DEFAULT_BATCH_SIZE)?;
                }
                if should_yield() {
                    break false;
                }
            }
        }
    };

    // Flush BEFORE reporting the offset: the checkpointed resume point must
    // never run ahead of what is durably in the index.
    nodes_indexed += insert_coords_batched(db, buffer.drain(..), DEFAULT_BATCH_SIZE)?;

    Ok(Pass1Slice {
        next_offset: scanner.offset(),
        finished,
        nodes_indexed,
        blocks_scanned,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::{DenseNodes, Node, PrimitiveGroup, StringTable};
    use crate::{coord_count, get_coord, open_coord_db};
    use std::fs;
    use std::path::PathBuf;

    fn tmp_dir(tag: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("freehike-scan-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// Frames `header` + `blob` bytes as `[u32 BE len][BlobHeader][Blob]`.
    fn frame(blob_type: &str, blob: &Blob) -> Vec<u8> {
        let blob_bytes = blob.encode_to_vec();
        let header = BlobHeader {
            r#type: blob_type.to_string(),
            indexdata: None,
            datasize: blob_bytes.len() as i32,
        };
        let header_bytes = header.encode_to_vec();
        let mut out = (header_bytes.len() as u32).to_be_bytes().to_vec();
        out.extend_from_slice(&header_bytes);
        out.extend_from_slice(&blob_bytes);
        out
    }

    /// Zlib-compressed OSMData blob from a PrimitiveBlock.
    fn data_blob(block: &PrimitiveBlock) -> Blob {
        let payload = block.encode_to_vec();
        Blob {
            raw: None,
            raw_size: Some(payload.len() as i32),
            zlib_data: Some(miniz_oxide::deflate::compress_to_vec_zlib(&payload, 6)),
        }
    }

    /// Delta-encodes absolute `(id, lat_units, lon_units)` triples into a
    /// DenseNodes-bearing PrimitiveBlock — the inverse of what
    /// `extract_node_coords` must perform.
    fn dense_block(nodes: &[(i64, i64, i64)], granularity: Option<i32>) -> PrimitiveBlock {
        let (mut pid, mut plat, mut plon) = (0i64, 0i64, 0i64);
        let mut dense = DenseNodes::default();
        for &(id, lat, lon) in nodes {
            dense.id.push(id - pid);
            dense.lat.push(lat - plat);
            dense.lon.push(lon - plon);
            (pid, plat, plon) = (id, lat, lon);
        }
        PrimitiveBlock {
            stringtable: Some(StringTable::default()),
            primitivegroup: vec![PrimitiveGroup {
                nodes: vec![],
                dense: Some(dense),
            }],
            granularity,
            lat_offset: None,
            lon_offset: None,
        }
    }

    /// Writes a synthetic-but-wire-valid PBF: OSMHeader + one OSMData block
    /// per node group.
    fn write_test_pbf(dir: &std::path::Path, groups: &[Vec<(i64, i64, i64)>]) -> PathBuf {
        let mut bytes = frame(
            "OSMHeader",
            &Blob {
                raw: Some(b"stub header block".to_vec()),
                raw_size: None,
                zlib_data: None,
            },
        );
        for group in groups {
            bytes.extend_from_slice(&frame("OSMData", &data_blob(&dense_block(group, None))));
        }
        let path = dir.join("test.osm.pbf");
        fs::write(&path, bytes).unwrap();
        path
    }

    /// Expected index value for granularity-unit coordinates, computed via
    /// the same projection path the decoder uses.
    fn expected(lat_units: i64, lon_units: i64) -> (f64, f64) {
        web_mercator(
            1e-9 * (100 * lon_units) as f64,
            1e-9 * (100 * lat_units) as f64,
        )
    }

    // Innsbruck-ish and hostile-ish fixtures, in default-granularity units
    // (1 unit = 1e-7 degrees). Includes backward ID jumps (negative deltas)
    // and southern/western hemisphere coordinates (negative absolutes).
    const GROUP_A: &[(i64, i64, i64)] = &[
        (1_000, 472_700_000, 113_900_000),
        (1_005, 472_700_100, 113_900_050),
        (900, -338_650_000, -703_450_000), // id goes BACKWARD; Valparaíso
    ];
    const GROUP_B: &[(i64, i64, i64)] = &[
        (2_000_000_000, 0, 0),
        (2_000_000_007, -900_000_000, 1_800_000_000), // pole-clamped, antimeridian
    ];

    #[test]
    fn pass1_full_run_delta_decodes_and_indexes_all_nodes() {
        let dir = tmp_dir("full");
        let pbf_path = write_test_pbf(&dir, &[GROUP_A.to_vec(), GROUP_B.to_vec()]);
        let pbf = PbfMmap::open(&pbf_path).unwrap();
        let db = open_coord_db(&dir.join("coords.redb")).unwrap();

        let slice = run_pass1_slice(&pbf, &db, 0, &mut || false).unwrap();
        assert!(slice.finished);
        assert_eq!(slice.blocks_scanned, 3, "1 header + 2 data");
        assert_eq!(slice.nodes_indexed, 5);
        assert_eq!(slice.next_offset, pbf.len(), "clean EOF lands on file end");
        assert_eq!(coord_count(&db).unwrap(), 5);

        for &(id, lat, lon) in GROUP_A.iter().chain(GROUP_B) {
            assert_eq!(
                get_coord(&db, id as u64).unwrap(),
                Some(expected(lat, lon)),
                "node {id}"
            );
        }
    }

    #[test]
    fn pass1_yields_at_block_boundaries_and_resumes_without_duplicates() {
        let dir = tmp_dir("yield");
        let pbf_path = write_test_pbf(&dir, &[GROUP_A.to_vec(), GROUP_B.to_vec()]);
        let pbf = PbfMmap::open(&pbf_path).unwrap();
        let db = open_coord_db(&dir.join("coords.redb")).unwrap();

        // Yield after every single block — worst-case slicing.
        let mut offsets = vec![0usize];
        let mut total_nodes = 0u64;
        let mut slices = 0;
        loop {
            let s = run_pass1_slice(&pbf, &db, *offsets.last().unwrap(), &mut || true).unwrap();
            // Minimum forward progress: exactly one block per slice — except
            // the final slice, which starts at EOF and only reports finished.
            assert!(
                s.blocks_scanned == 1 || (s.finished && s.blocks_scanned == 0),
                "slice made no progress: {s:?}"
            );
            total_nodes += s.nodes_indexed;
            slices += 1;
            offsets.push(s.next_offset);
            if s.finished {
                break;
            }
            assert!(slices < 100, "did not finish");
        }

        assert_eq!(slices, 4, "3 blocks + 1 EOF-detecting slice");
        assert_eq!(
            total_nodes, 5,
            "per-slice sums must equal distinct nodes (no re-scan duplication)"
        );
        assert_eq!(coord_count(&db).unwrap(), 5);
        assert!(
            offsets.windows(2).all(|w| w[0] <= w[1]),
            "offsets monotonic: {offsets:?}"
        );
        // Node from the LAST data block must be present — proves the final
        // flush-before-return, not just the batch-threshold path.
        assert!(get_coord(&db, 2_000_000_007).unwrap().is_some());
    }

    #[test]
    fn plain_nodes_and_custom_granularity_extracted() {
        let dir = tmp_dir("plain");
        // granularity=1000 with lat/lon offsets exercises the full formula.
        let block = PrimitiveBlock {
            stringtable: None,
            primitivegroup: vec![PrimitiveGroup {
                nodes: vec![Node {
                    id: 7,
                    lat: 47_270_000,
                    lon: 11_390_000,
                }],
                dense: None,
            }],
            granularity: Some(1_000),
            lat_offset: Some(500),
            lon_offset: Some(-500),
        };
        let mut bytes = frame(
            "OSMHeader",
            &Blob {
                raw: Some(b"h".to_vec()),
                raw_size: None,
                zlib_data: None,
            },
        );
        bytes.extend_from_slice(&frame("OSMData", &data_blob(&block)));
        let path = dir.join("plain.osm.pbf");
        fs::write(&path, bytes).unwrap();

        let pbf = PbfMmap::open(&path).unwrap();
        let db = open_coord_db(&dir.join("coords.redb")).unwrap();
        let slice = run_pass1_slice(&pbf, &db, 0, &mut || false).unwrap();
        assert_eq!(slice.nodes_indexed, 1);
        let want = web_mercator(
            1e-9 * (-500i64 + 1_000 * 11_390_000) as f64,
            1e-9 * (500i64 + 1_000 * 47_270_000) as f64,
        );
        assert_eq!(get_coord(&db, 7).unwrap(), Some(want));
    }

    #[test]
    fn unknown_block_types_are_skipped_not_fatal() {
        let dir = tmp_dir("unknown");
        let mut bytes = frame(
            "FancyFutureIndex",
            &Blob {
                raw: Some(vec![1, 2, 3]),
                raw_size: None,
                zlib_data: None,
            },
        );
        bytes.extend_from_slice(&frame("OSMData", &data_blob(&dense_block(GROUP_A, None))));
        let path = dir.join("unknown.osm.pbf");
        fs::write(&path, bytes).unwrap();

        let pbf = PbfMmap::open(&path).unwrap();
        let db = open_coord_db(&dir.join("coords.redb")).unwrap();
        let slice = run_pass1_slice(&pbf, &db, 0, &mut || false).unwrap();
        assert!(slice.finished);
        assert_eq!(slice.nodes_indexed, GROUP_A.len() as u64);
    }

    #[test]
    fn hostile_and_truncated_framing_rejected() {
        let dir = tmp_dir("hostile");
        let db = open_coord_db(&dir.join("coords.redb")).unwrap();

        let cases: Vec<(&str, Vec<u8>)> = vec![
            (
                "html-as-pbf",
                b"<!DOCTYPE html><html>ceci n'est pas un pbf</html>".to_vec(),
            ),
            ("truncated-prefix", vec![0x00, 0x00]),
            ("truncated-blob", {
                let full = frame("OSMData", &data_blob(&dense_block(GROUP_A, None)));
                full[..full.len() - 5].to_vec()
            }),
            ("zero-header-len", vec![0, 0, 0, 0, 1, 2, 3]),
        ];
        for (name, bytes) in cases {
            let path = dir.join(format!("{name}.pbf"));
            fs::write(&path, &bytes).unwrap();
            let pbf = PbfMmap::open(&path).unwrap();
            match run_pass1_slice(&pbf, &db, 0, &mut || false) {
                Err(IndexError::InvalidInput(msg)) => {
                    assert!(msg.contains("corrupted PBF"), "{name}: {msg}")
                }
                other => panic!("{name}: expected InvalidInput, got {other:?}"),
            }
        }
        assert_eq!(
            coord_count(&db).unwrap(),
            0,
            "nothing committed from garbage"
        );
    }

    #[test]
    fn unsupported_compression_and_size_lies_rejected() {
        let dir = tmp_dir("badblob");
        let db = open_coord_db(&dir.join("coords.redb")).unwrap();

        // Neither raw nor zlib_data.
        let empty = frame(
            "OSMData",
            &Blob {
                raw: None,
                raw_size: Some(10),
                zlib_data: None,
            },
        );
        // raw_size disagrees with the actual inflated size.
        let payload = dense_block(GROUP_A, None).encode_to_vec();
        let lying = frame(
            "OSMData",
            &Blob {
                raw: None,
                raw_size: Some(payload.len() as i32 + 999),
                zlib_data: Some(miniz_oxide::deflate::compress_to_vec_zlib(&payload, 6)),
            },
        );
        for (name, bytes) in [("no-encoding", empty), ("raw-size-lie", lying)] {
            let path = dir.join(format!("{name}.pbf"));
            fs::write(&path, &bytes).unwrap();
            let pbf = PbfMmap::open(&path).unwrap();
            assert!(
                matches!(
                    run_pass1_slice(&pbf, &db, 0, &mut || false),
                    Err(IndexError::InvalidInput(_))
                ),
                "{name} must be rejected"
            );
        }
    }

    #[test]
    fn mismatched_dense_arrays_rejected() {
        let mut block = dense_block(GROUP_A, None);
        block.primitivegroup[0].dense.as_mut().unwrap().lat.pop();
        let mut out = Vec::new();
        match extract_node_coords(&block, &mut out) {
            Err(IndexError::InvalidInput(msg)) => {
                assert!(msg.contains("parallel arrays"), "got: {msg}")
            }
            other => panic!("expected InvalidInput, got {other:?}"),
        }
    }

    #[test]
    fn stringtable_prefilter_detects_relevant_keys() {
        // Way-block-shaped stringtable: contains a hiking-relevant key.
        let mut relevant = dense_block(GROUP_A, None);
        relevant.stringtable = Some(StringTable {
            s: vec![b"".to_vec(), b"name".to_vec(), b"sac_scale".to_vec()],
        });
        assert!(stringtable_has_relevant_keys(&relevant));

        // Typical node-block stringtable: no way keys — the exact case that
        // proves the filter must not gate Pass 1 (these nodes are still
        // referenced by ways elsewhere).
        let mut irrelevant = dense_block(GROUP_A, None);
        irrelevant.stringtable = Some(StringTable {
            s: vec![b"".to_vec(), b"created_by".to_vec()],
        });
        assert!(!stringtable_has_relevant_keys(&irrelevant));
        assert!(!stringtable_has_relevant_keys(&dense_block(GROUP_A, None)));

        // Key must match whole, not as substring.
        let mut substring = dense_block(GROUP_A, None);
        substring.stringtable = Some(StringTable {
            s: vec![b"highway_lamp_ref".to_vec()],
        });
        assert!(!stringtable_has_relevant_keys(&substring));
    }

    #[test]
    fn resume_offset_past_eof_rejected() {
        let dir = tmp_dir("badresume");
        let pbf_path = write_test_pbf(&dir, &[GROUP_A.to_vec()]);
        let pbf = PbfMmap::open(&pbf_path).unwrap();
        let db = open_coord_db(&dir.join("coords.redb")).unwrap();
        match run_pass1_slice(&pbf, &db, pbf.len() + 1, &mut || false) {
            Err(IndexError::InvalidInput(msg)) => {
                assert!(msg.contains("beyond end"), "got: {msg}")
            }
            other => panic!("expected InvalidInput, got {other:?}"),
        }
        // Exactly-at-EOF is the legitimate "already finished" resume.
        let s = run_pass1_slice(&pbf, &db, pbf.len(), &mut || false).unwrap();
        assert!(s.finished);
        assert_eq!(s.nodes_indexed, 0);
    }

    /// Full Pass 1 over the real Innsbruck extract (~19.5MB Geofabrik-derived
    /// fixture), sliced with forced yields, proving the decoder on real-world
    /// data. Ignored by default so the L1 ladder stays fixture-independent;
    /// run explicitly with:
    ///   cargo test -p pbf -- --ignored --nocapture pass1_real
    #[test]
    #[ignore]
    fn pass1_real_innsbruck_extract() {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../offline_sandbox/raw_data/innsbruck.osm.pbf");
        let pbf = PbfMmap::open(&fixture).expect("fixture missing — see path in test");
        let dir = tmp_dir("real-innsbruck");
        let db = open_coord_db(&dir.join("coords.redb")).unwrap();

        // Yield every 4 blocks: exercises resume on real data, not just toys.
        let mut offset = 0usize;
        let mut total_nodes = 0u64;
        let mut total_blocks = 0u32;
        let mut slices = 0u32;
        loop {
            let mut in_slice = 0u32;
            let s = run_pass1_slice(&pbf, &db, offset, &mut || {
                in_slice += 1;
                in_slice >= 4
            })
            .unwrap();
            total_nodes += s.nodes_indexed;
            total_blocks += s.blocks_scanned;
            slices += 1;
            offset = s.next_offset;
            if s.finished {
                break;
            }
            assert!(slices < 10_000, "runaway");
        }

        assert_eq!(offset, pbf.len());
        assert!(slices > 1, "fixture should take multiple slices");
        assert!(
            total_nodes > 100_000,
            "Innsbruck extract implausibly small: {total_nodes} nodes"
        );
        assert_eq!(
            coord_count(&db).unwrap(),
            total_nodes,
            "per-slice sums must equal distinct indexed nodes (no duplication)"
        );
        println!(
            "real-extract pass1: {total_nodes} nodes / {total_blocks} blocks / {slices} slices"
        );
    }
}
