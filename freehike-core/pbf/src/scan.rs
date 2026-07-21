// SPDX-License-Identifier: Apache-2.0
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

use crate::proto::{Blob, BlobHeader, PrimitiveBlock, StringTable, StringTableProbe, WayBlock};
use crate::{
    insert_coords_batched, insert_ways_batched, web_mercator, IndexError, IndexedWay, PbfMmap,
    DEFAULT_BATCH_SIZE, LAYER_KEYS,
};

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

    /// Advances over the next `[len][BlobHeader][Blob]` record, returning its
    /// framing and undecoded blob bytes. `Ok(None)` = clean end of file.
    fn next_raw(&mut self) -> Result<Option<RawBlock<'a>>, IndexError> {
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

        self.offset = end_offset;
        Ok(Some(RawBlock {
            start_offset: start,
            end_offset,
            blob_offset: blob_start,
            r#type: header.r#type,
            blob_bytes,
        }))
    }

    /// Decodes the next block through Pass 1's node-bearing view.
    /// `Ok(None)` = clean end of file.
    pub fn next_block(&mut self) -> Result<Option<ScannedBlock>, IndexError> {
        let Some(raw) = self.next_raw()? else {
            return Ok(None);
        };
        let kind =
            match raw.r#type.as_str() {
                "OSMHeader" => BlockKind::Header,
                "OSMData" => {
                    let payload = decompress_blob(raw.blob_bytes, raw.blob_offset)?;
                    BlockKind::Data(PrimitiveBlock::decode(payload.as_slice()).map_err(|e| {
                        corrupt(raw.blob_offset, format!("PrimitiveBlock decode: {e}"))
                    })?)
                }
                other => BlockKind::Skipped(other.to_string()),
            };
        Ok(Some(ScannedBlock {
            start_offset: raw.start_offset,
            end_offset: raw.end_offset,
            kind,
        }))
    }

    /// Decodes the next block through Pass 2's way-bearing view, applying the
    /// StringTable semantic pre-filter: a data block whose StringTable lacks
    /// every relevant tag key returns `WayScan::Irrelevant` after decoding
    /// ONLY the probe view — its Way messages are wire-skipped, never
    /// materialized (the Blueprint's "skipped without incurring the CPU cost
    /// of deserializing the individual entities").
    pub fn next_way_block(&mut self) -> Result<Option<WayScan>, IndexError> {
        let Some(raw) = self.next_raw()? else {
            return Ok(None);
        };
        if raw.r#type != "OSMData" {
            return Ok(Some(WayScan::NotData));
        }
        let payload = decompress_blob(raw.blob_bytes, raw.blob_offset)?;
        let probe = StringTableProbe::decode(payload.as_slice())
            .map_err(|e| corrupt(raw.blob_offset, format!("StringTable probe decode: {e}")))?;
        let relevant = probe
            .stringtable
            .as_ref()
            .is_some_and(stringtable_is_relevant);
        if !relevant {
            return Ok(Some(WayScan::Irrelevant));
        }
        let block = WayBlock::decode(payload.as_slice())
            .map_err(|e| corrupt(raw.blob_offset, format!("WayBlock decode: {e}")))?;
        Ok(Some(WayScan::Relevant(block)))
    }
}

/// One undecoded framing record (internal to the two typed views above).
struct RawBlock<'a> {
    start_offset: usize,
    end_offset: usize,
    /// Offset of the blob payload, for error attribution.
    blob_offset: usize,
    r#type: String,
    blob_bytes: &'a [u8],
}

/// Pass-2 classification of one scanned block.
#[derive(Debug)]
pub enum WayScan {
    /// OSMHeader or an unknown blob type — nothing for Pass 2 here.
    NotData,
    /// Data block rejected by the StringTable pre-filter (ways not decoded).
    Irrelevant,
    /// Data block that may contain renderable ways.
    Relevant(WayBlock),
}

/// Blob → decompressed payload bytes (both passes decode their own prost
/// view of the result). The decompression output size is capped by the
/// declared `raw_size` (itself capped), so a zlib bomb cannot blow the RAM
/// budget.
fn decompress_blob(blob_bytes: &[u8], offset: usize) -> Result<Vec<u8>, IndexError> {
    let blob =
        Blob::decode(blob_bytes).map_err(|e| corrupt(offset, format!("Blob decode: {e}")))?;

    if let Some(raw) = blob.raw {
        Ok(raw)
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
        Ok(out)
    } else {
        Err(corrupt(
            offset,
            "unsupported blob encoding (only raw and zlib_data are supported)",
        ))
    }
}

// ---------------------------------------------------------------------------
// StringTable semantic pre-filter (Blueprint: "Semantic Filtering")
// ---------------------------------------------------------------------------

/// Tag keys that make a block semantically relevant to the hiking pipeline,
/// per the Blueprint ("sac_scale for trail difficulty, highway for paths,
/// waterway for streams, or contour-related tags") and the architecture plan
/// (research/, Phase 3/4). Superset of [`crate::LAYER_KEYS`]: this list gates
/// BLOCKS (conservative — a sac_scale-bearing block is worth deserializing),
/// while the way-level keep-filter requires an actual layer key (P5.C2 —
/// un-layered geometry cannot be styled and no longer has a storage form).
pub const RELEVANT_TAG_KEYS: &[&[u8]] = &[
    b"highway",
    b"sac_scale",
    b"waterway",
    b"natural",
    b"ele",
    b"landuse",
];

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
    block
        .stringtable
        .as_ref()
        .is_some_and(stringtable_is_relevant)
}

/// Core of the pre-filter, usable from any block view (Pass 2 applies it to
/// the [`StringTableProbe`] before ways are ever deserialized).
pub fn stringtable_is_relevant(st: &StringTable) -> bool {
    st.s.iter()
        .any(|s| RELEVANT_TAG_KEYS.contains(&s.as_slice()))
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
    let mut poi_buffer: Vec<(u64, (f64, f64), Vec<u8>)> = Vec::new();
    let mut nodes_indexed = 0u64;
    let mut blocks_scanned = 0u32;

    let finished = loop {
        match scanner.next_block()? {
            None => break true,
            Some(block) => {
                if let BlockKind::Data(pb) = &block.kind {
                    extract_node_coords(pb, &mut buffer)?;
                    // Node-POI extraction (P-CORE.C8, closes D002), gated
                    // behind a StringTable probe so peak-free blocks never
                    // pay for keys_vals decoding.
                    if block_mentions_peak(pb) {
                        extract_peak_pois(pb, &mut poi_buffer)?;
                    }
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
    // never run ahead of what is durably in the index. POIs flush after
    // coords under the same rule; both upsert idempotently on re-scan.
    nodes_indexed += insert_coords_batched(db, buffer.drain(..), DEFAULT_BATCH_SIZE)?;
    crate::insert_pois(db, &poi_buffer)?;

    Ok(Pass1Slice {
        next_offset: scanner.offset(),
        finished,
        nodes_indexed,
        blocks_scanned,
    })
}

/// StringTable relevance probe for the POI pass: only a block whose table
/// interns both `natural` and `peak` can hold a peak node — everything
/// else skips keys_vals decoding entirely (the P2 prefilter discipline).
fn block_mentions_peak(block: &PrimitiveBlock) -> bool {
    match &block.stringtable {
        Some(st) => st.s.iter().any(|s| s == b"natural") && st.s.iter().any(|s| s == b"peak"),
        None => false,
    }
}

/// Walks DenseNodes `keys_vals` in parallel with the delta-decoded id/coord
/// arrays and collects `natural=peak` nodes as `(id, (x, y), name)`.
///
/// `keys_vals` per spec: for each node in order, `(key_idx, val_idx)` pairs
/// terminated by one `0`; an entirely-untagged group omits the array. An
/// exhausted stream simply leaves the remaining nodes untagged (some
/// writers trim trailing zeros); an OUT-OF-RANGE StringTable index is a
/// typed error — that is corruption, not style.
fn extract_peak_pois(
    block: &PrimitiveBlock,
    out: &mut Vec<(u64, (f64, f64), Vec<u8>)>,
) -> Result<(), IndexError> {
    let Some(st) = &block.stringtable else {
        return Ok(());
    };
    let granularity = i64::from(block.granularity.unwrap_or(100));
    let lat_offset = block.lat_offset.unwrap_or(0);
    let lon_offset = block.lon_offset.unwrap_or(0);
    let project = |lon_units: i64, lat_units: i64| {
        web_mercator(
            1e-9 * (lon_offset + granularity * lon_units) as f64,
            1e-9 * (lat_offset + granularity * lat_units) as f64,
        )
    };
    let string_at = |idx: i32| -> Result<&[u8], IndexError> {
        usize::try_from(idx)
            .ok()
            .and_then(|i| st.s.get(i))
            .map(Vec::as_slice)
            .ok_or_else(|| {
                IndexError::InvalidInput(format!(
                    "corrupted DenseNodes: keys_vals index {idx} outside StringTable ({} entries)",
                    st.s.len()
                ))
            })
    };

    for group in &block.primitivegroup {
        let Some(dense) = &group.dense else { continue };
        if dense.keys_vals.is_empty() {
            continue;
        }
        let (mut id, mut lat, mut lon) = (0i64, 0i64, 0i64);
        let mut kv = dense.keys_vals.iter();
        for i in 0..dense.id.len() {
            id += dense.id[i];
            lat += dense.lat[i];
            lon += dense.lon[i];

            let mut is_peak = false;
            let mut name: Vec<u8> = Vec::new();
            while let Some(&key_idx) = kv.next() {
                if key_idx == 0 {
                    break; // this node's tag list ends
                }
                let Some(&val_idx) = kv.next() else { break };
                let key = string_at(key_idx)?;
                let val = string_at(val_idx)?;
                if key == b"natural" && val == b"peak" {
                    is_peak = true;
                } else if key == b"name" {
                    name = val.to_vec();
                }
            }
            if is_peak {
                let node_id = u64::try_from(id).map_err(|_| {
                    IndexError::InvalidInput(format!("corrupted DenseNodes: negative node id {id}"))
                })?;
                out.push((node_id, project(lon, lat), name));
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Pass 2 driver (way extraction, budget-yieldable)
// ---------------------------------------------------------------------------

/// Extracts renderable ways from a pre-filter-surviving block: keeps a way
/// iff one of its tag keys resolves (via the StringTable) to a member of
/// [`crate::LAYER_KEYS`], resolving the way's `(layer, class)` from the
/// highest-priority matching key — plus the optional `sac_scale`
/// difficulty grade for highway-layer ways (P5.C3) and the optional
/// `name` label for ANY layer (P5.C4) — then delta-decodes its node refs.
/// Degenerate ways (< 2 refs) are dropped — they cannot form geometry.
///
/// `keys`/`vals` are parallel arrays per the OSM wire spec; a length
/// mismatch, an index outside the StringTable, or a non-UTF-8 class value
/// are typed corruption errors (stringtable strings are UTF-8 by spec).
pub fn extract_relevant_ways(
    block: &WayBlock,
    out: &mut Vec<IndexedWay>,
) -> Result<u64, IndexError> {
    let Some(st) = block.stringtable.as_ref() else {
        return Ok(0); // no stringtable → no resolvable tags → nothing renderable
    };
    let mut extracted = 0u64;
    for group in &block.primitivegroup {
        for way in &group.ways {
            if way.keys.len() != way.vals.len() {
                return Err(IndexError::InvalidInput(format!(
                    "corrupted way {}: {} keys but {} vals (parallel arrays)",
                    way.id,
                    way.keys.len(),
                    way.vals.len()
                )));
            }

            // Highest-priority layer key wins (e.g. highway over natural on
            // a way carrying both). sac_scale's and name's value indices
            // are noted in the same sweep — sac_scale resolved below only
            // when the way turns out to be a highway (the only layer the
            // grade applies to); name resolved for EVERY layer (P5.C4).
            let mut best: Option<(u8, u32)> = None; // (layer idx, val idx)
            let mut sac_val_idx: Option<u32> = None;
            let mut name_val_idx: Option<u32> = None;
            for (&key_idx, &val_idx) in way.keys.iter().zip(&way.vals) {
                let key = st.s.get(key_idx as usize).ok_or_else(|| {
                    IndexError::InvalidInput(format!(
                        "corrupted way {}: key index {key_idx} outside StringTable (len {})",
                        way.id,
                        st.s.len()
                    ))
                })?;
                if let Some(layer) = LAYER_KEYS.iter().position(|k| *k == key.as_slice()) {
                    let layer = layer as u8;
                    if best.is_none_or(|(l, _)| layer < l) {
                        best = Some((layer, val_idx));
                    }
                } else if key.as_slice() == b"sac_scale" {
                    sac_val_idx = Some(val_idx);
                } else if key.as_slice() == b"name" {
                    name_val_idx = Some(val_idx);
                }
            }
            let Some((layer, val_idx)) = best else {
                continue; // no layer key → not styleable → dropped
            };
            if way.refs.len() < 2 {
                continue;
            }

            // Shared resolver: bounds-check the StringTable index, then
            // insist on UTF-8 (stringtable strings are UTF-8 by spec — a
            // violation is corruption, not data).
            let resolve = |val_idx: u32, what: &str| -> Result<&Vec<u8>, IndexError> {
                let value = st.s.get(val_idx as usize).ok_or_else(|| {
                    IndexError::InvalidInput(format!(
                        "corrupted way {}: {what} value index {val_idx} outside StringTable (len {})",
                        way.id,
                        st.s.len()
                    ))
                })?;
                if std::str::from_utf8(value).is_err() {
                    return Err(IndexError::InvalidInput(format!(
                        "corrupted way {}: non-UTF-8 {what} value in StringTable",
                        way.id
                    )));
                }
                Ok(value)
            };
            let class = resolve(val_idx, "class")?;
            let sac_scale = match sac_val_idx {
                Some(idx) if layer == 0 => resolve(idx, "sac_scale")?.clone(),
                _ => Vec::new(), // absent, or a grade on a non-highway way
            };
            let name = match name_val_idx {
                Some(idx) => resolve(idx, "name")?.clone(),
                None => Vec::new(),
            };

            let way_id = u64::try_from(way.id).map_err(|_| {
                IndexError::InvalidInput(format!("corrupted way: negative id {}", way.id))
            })?;
            // Delta decoding: refs are sint64 diffs from the predecessor.
            let mut refs = Vec::with_capacity(way.refs.len());
            let mut node: i64 = 0;
            for &delta in &way.refs {
                node = node.checked_add(delta).ok_or_else(|| {
                    IndexError::InvalidInput(format!("corrupted way {way_id}: ref delta overflow"))
                })?;
                let id = u64::try_from(node).map_err(|_| {
                    IndexError::InvalidInput(format!(
                        "corrupted way {way_id}: negative node ref {node}"
                    ))
                })?;
                refs.push(id);
            }
            out.push(IndexedWay {
                id: way_id,
                layer,
                class: class.clone(),
                sac_scale,
                name,
                refs,
            });
            extracted += 1;
        }
    }
    Ok(extracted)
}

/// Result of one Pass-2 slice. Same durable contract as [`Pass1Slice`]:
/// every way from blocks before `next_offset` is committed before return.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pass2Slice {
    /// Block-boundary byte offset to resume from (checkpoint this).
    pub next_offset: usize,
    /// True when the whole file has been scanned.
    pub finished: bool,
    /// Renderable ways committed by THIS slice.
    pub ways_indexed: u64,
    /// Blocks consumed by this slice (header + skipped + data).
    pub blocks_scanned: u32,
    /// Data blocks rejected by the StringTable pre-filter (ways never
    /// deserialized) — the Blueprint optimization, observable for telemetry.
    pub blocks_prefiltered: u32,
}

/// Runs Pass 2 (way extraction) from `resume_offset` — its own cursor,
/// independent of Pass 1's; a fresh Pass 2 starts at 0 and re-walks the same
/// framing through the way-bearing view. Yield/min-progress/flush contracts
/// are identical to [`run_pass1_slice`].
pub fn run_pass2_slice(
    pbf: &PbfMmap,
    db: &redb::Database,
    resume_offset: usize,
    should_yield: &mut dyn FnMut() -> bool,
) -> Result<Pass2Slice, IndexError> {
    let mut scanner = BlockScanner::resume(pbf, resume_offset);
    let mut buffer: Vec<IndexedWay> = Vec::new();
    let mut ways_indexed = 0u64;
    let mut blocks_scanned = 0u32;
    let mut blocks_prefiltered = 0u32;

    let finished = loop {
        match scanner.next_way_block()? {
            None => break true,
            Some(scan) => {
                match &scan {
                    WayScan::Relevant(block) => {
                        extract_relevant_ways(block, &mut buffer)?;
                    }
                    WayScan::Irrelevant => blocks_prefiltered += 1,
                    WayScan::NotData => {}
                }
                blocks_scanned += 1;
                if buffer.len() >= DEFAULT_BATCH_SIZE {
                    ways_indexed += insert_ways_batched(db, buffer.drain(..), DEFAULT_BATCH_SIZE)?;
                }
                if should_yield() {
                    break false;
                }
            }
        }
    };

    // Flush BEFORE reporting the offset (same durability rule as Pass 1).
    ways_indexed += insert_ways_batched(db, buffer.drain(..), DEFAULT_BATCH_SIZE)?;

    Ok(Pass2Slice {
        next_offset: scanner.offset(),
        finished,
        ways_indexed,
        blocks_scanned,
        blocks_prefiltered,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{data_blob, dense_block, frame, synthetic_pbf};
    use crate::proto::{Node, PrimitiveGroup, StringTable};
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

    /// Writes a synthetic-but-wire-valid PBF: OSMHeader + one OSMData block
    /// per node group. (Builders live in `crate::fixtures`, shared with the
    /// compiler/ffi test suites.)
    fn write_test_pbf(dir: &std::path::Path, groups: &[&[(i64, i64, i64)]]) -> PathBuf {
        let path = dir.join("test.osm.pbf");
        fs::write(&path, synthetic_pbf(groups)).unwrap();
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
        let pbf_path = write_test_pbf(&dir, &[GROUP_A, GROUP_B]);
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
        let pbf_path = write_test_pbf(&dir, &[GROUP_A, GROUP_B]);
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
        let pbf_path = write_test_pbf(&dir, &[GROUP_A]);
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

    // -- Pass 2: way extraction + geometry assembly -----------------------

    use crate::fixtures::{synthetic_pbf_with_ways, FixtureWay};
    use crate::{assemble_way_geometry, get_way_refs, insert_ways_batched, IndexedWay};

    /// Nodes 1000/1005/900 (GROUP_A) + ways over them. Way 501 is tagged
    /// `building` — present in the StringTable but NOT a relevant key, so the
    /// block survives the pre-filter while the way itself is dropped.
    /// Way 503 is tagged `sac_scale` only: its key keeps the BLOCK relevant
    /// (prefilter) but carries no LAYER key, so the way itself is dropped —
    /// the P5.C2 tightened keep-rule.
    const WAYS_RELEVANT: &[FixtureWay<'static>] = &[
        (500, b"highway", b"path", &[1_000, 1_005, 900]),
        (501, b"building", b"yes", &[1_000, 900]),
        (502, b"waterway", b"stream", &[900, 1_005]),
        (503, b"sac_scale", b"hiking", &[1_000, 900]),
    ];
    /// A block whose StringTable holds no relevant key at all → prefiltered.
    const WAYS_IRRELEVANT: &[FixtureWay<'static>] = &[(600, b"created_by", b"JOSM", &[1_000, 900])];

    fn indexed_fixture(dir: &std::path::Path) -> (PbfMmap, redb::Database) {
        let path = dir.join("two-pass.osm.pbf");
        fs::write(
            &path,
            synthetic_pbf_with_ways(&[GROUP_A], &[WAYS_RELEVANT, WAYS_IRRELEVANT]),
        )
        .unwrap();
        let pbf = PbfMmap::open(&path).unwrap();
        let db = open_coord_db(&dir.join("index.redb")).unwrap();
        // Pass 1 populates Coordinates (way blocks contribute zero nodes).
        let p1 = run_pass1_slice(&pbf, &db, 0, &mut || false).unwrap();
        assert!(p1.finished);
        assert_eq!(p1.nodes_indexed, GROUP_A.len() as u64);
        (pbf, db)
    }

    #[test]
    fn pass2_filters_and_stores_relevant_ways() {
        let dir = tmp_dir("pass2");
        let (pbf, db) = indexed_fixture(&dir);

        let s = run_pass2_slice(&pbf, &db, 0, &mut || false).unwrap();
        assert!(s.finished);
        assert_eq!(s.next_offset, pbf.len());
        // header + 1 node block + 2 way blocks = 4 blocks scanned;
        // prefiltered: the node block (empty stringtable) + WAYS_IRRELEVANT.
        assert_eq!(s.blocks_scanned, 4);
        assert_eq!(s.blocks_prefiltered, 2);
        // Ways 500 + 502 kept; 501 (building) tag-filtered; 503 (sac_scale
        // only — no layer key) dropped by the P5.C2 rule; 600 prefiltered.
        assert_eq!(s.ways_indexed, 2);

        assert_eq!(
            get_way_refs(&db, 500).unwrap(),
            Some(vec![1_000, 1_005, 900])
        );
        assert_eq!(get_way_refs(&db, 502).unwrap(), Some(vec![900, 1_005]));
        assert_eq!(get_way_refs(&db, 501).unwrap(), None, "building filtered");
        assert_eq!(get_way_refs(&db, 503).unwrap(), None, "no layer key");
        assert_eq!(get_way_refs(&db, 600).unwrap(), None, "block prefiltered");

        // Tag metadata persisted in the same transaction as the refs
        // (single-tag fixture ways: no sac_scale/name → empty slots).
        assert_eq!(
            crate::get_way_tags(&db, 500).unwrap(),
            Some((0, b"path".to_vec(), Vec::new(), Vec::new())),
            "highway=path"
        );
        assert_eq!(
            crate::get_way_tags(&db, 502).unwrap(),
            Some((1, b"stream".to_vec(), Vec::new(), Vec::new())),
            "waterway=stream"
        );
        assert_eq!(crate::get_way_tags(&db, 501).unwrap(), None);
    }

    /// P5.C4: `name` is extracted for EVERY layer (the scope difference vs
    /// sac_scale) and coexists with the other attributes; unnamed ways keep
    /// the empty sentinel.
    #[test]
    fn pass2_extracts_name_on_all_layers() {
        use crate::proto::{Way, WayGroup};
        let block = WayBlock {
            stringtable: Some(StringTable {
                s: vec![
                    b"".to_vec(),
                    b"highway".to_vec(),
                    b"path".to_vec(),
                    b"sac_scale".to_vec(),
                    b"alpine_hiking".to_vec(),
                    b"name".to_vec(),
                    "Goetheweg".as_bytes().to_vec(),
                    b"waterway".to_vec(),
                    b"river".to_vec(),
                    "Inn".as_bytes().to_vec(),
                ],
            }),
            primitivegroup: vec![WayGroup {
                ways: vec![
                    // Fully-attributed trail: class + grade + name.
                    Way {
                        id: 1,
                        keys: vec![1, 3, 5],
                        vals: vec![2, 4, 6],
                        refs: vec![10, 1],
                    },
                    // Named river — name on a NON-highway layer.
                    Way {
                        id: 2,
                        keys: vec![7, 5],
                        vals: vec![8, 9],
                        refs: vec![20, 1],
                    },
                    // Unnamed path.
                    Way {
                        id: 3,
                        keys: vec![1],
                        vals: vec![2],
                        refs: vec![30, 1],
                    },
                ],
            }],
        };
        let mut out = Vec::new();
        assert_eq!(extract_relevant_ways(&block, &mut out).unwrap(), 3);
        assert_eq!(
            (out[0].sac_scale.as_slice(), out[0].name.as_slice()),
            (&b"alpine_hiking"[..], "Goetheweg".as_bytes()),
        );
        assert_eq!(
            (out[1].layer, out[1].name.as_slice()),
            (1, "Inn".as_bytes()),
            "name extracted on the waterway layer"
        );
        assert_eq!(out[2].name, Vec::<u8>::new());
    }

    /// name corruption is typed like every other tag failure.
    #[test]
    fn corrupted_name_rejected() {
        use crate::proto::{Way, WayGroup};
        let mut bad = WayBlock {
            stringtable: Some(StringTable {
                s: vec![
                    b"".to_vec(),
                    b"highway".to_vec(),
                    b"path".to_vec(),
                    b"name".to_vec(),
                    b"Weg".to_vec(),
                ],
            }),
            primitivegroup: vec![WayGroup {
                ways: vec![Way {
                    id: 1,
                    keys: vec![1, 3],
                    vals: vec![2, 9], // name value index outside the StringTable
                    refs: vec![10, 1],
                }],
            }],
        };
        let mut out = Vec::new();
        assert!(matches!(
            extract_relevant_ways(&bad, &mut out),
            Err(IndexError::InvalidInput(_))
        ));

        // Non-UTF-8 name.
        bad.primitivegroup[0].ways[0].vals = vec![2, 4];
        bad.stringtable.as_mut().unwrap().s[4] = vec![0xff, 0xfe];
        assert!(matches!(
            extract_relevant_ways(&bad, &mut out),
            Err(IndexError::InvalidInput(_))
        ));
    }

    /// P5.C3: sac_scale rides along on highway ways; on a non-highway way
    /// the grade is nonsense data and is deliberately ignored.
    #[test]
    fn pass2_extracts_sac_scale_on_highways_only() {
        use crate::proto::{Way, WayGroup};
        let block = WayBlock {
            stringtable: Some(StringTable {
                s: vec![
                    b"".to_vec(),
                    b"highway".to_vec(),
                    b"path".to_vec(),
                    b"sac_scale".to_vec(),
                    b"demanding_mountain_hiking".to_vec(),
                    b"waterway".to_vec(),
                    b"stream".to_vec(),
                ],
            }),
            primitivegroup: vec![WayGroup {
                ways: vec![
                    // highway=path + sac_scale=demanding_mountain_hiking.
                    Way {
                        id: 1,
                        keys: vec![1, 3],
                        vals: vec![2, 4],
                        refs: vec![10, 1],
                    },
                    // highway=path, no grade.
                    Way {
                        id: 2,
                        keys: vec![1],
                        vals: vec![2],
                        refs: vec![20, 1],
                    },
                    // waterway=stream + (bogus) sac_scale → grade ignored.
                    Way {
                        id: 3,
                        keys: vec![5, 3],
                        vals: vec![6, 4],
                        refs: vec![30, 1],
                    },
                ],
            }],
        };
        let mut out = Vec::new();
        assert_eq!(extract_relevant_ways(&block, &mut out).unwrap(), 3);
        assert_eq!(out[0].sac_scale, b"demanding_mountain_hiking".to_vec());
        assert_eq!(out[1].sac_scale, Vec::<u8>::new());
        assert_eq!(
            (out[2].layer, out[2].sac_scale.as_slice()),
            (1, &b""[..]),
            "grade on a waterway is dropped"
        );
    }

    /// sac_scale corruption is typed like every other tag failure.
    #[test]
    fn corrupted_sac_scale_rejected() {
        use crate::proto::{Way, WayGroup};
        let mut bad = WayBlock {
            stringtable: Some(StringTable {
                s: vec![
                    b"".to_vec(),
                    b"highway".to_vec(),
                    b"path".to_vec(),
                    b"sac_scale".to_vec(),
                    b"hiking".to_vec(),
                ],
            }),
            primitivegroup: vec![WayGroup {
                ways: vec![Way {
                    id: 1,
                    keys: vec![1, 3],
                    vals: vec![2, 9], // sac value index outside the StringTable
                    refs: vec![10, 1],
                }],
            }],
        };
        let mut out = Vec::new();
        assert!(matches!(
            extract_relevant_ways(&bad, &mut out),
            Err(IndexError::InvalidInput(_))
        ));

        // Non-UTF-8 grade.
        bad.primitivegroup[0].ways[0].vals = vec![2, 4];
        bad.stringtable.as_mut().unwrap().s[4] = vec![0xff, 0xfe];
        assert!(matches!(
            extract_relevant_ways(&bad, &mut out),
            Err(IndexError::InvalidInput(_))
        ));
    }

    /// A way carrying multiple layer keys resolves to the highest-priority
    /// one (highway wins over natural), and landuse is now a keeper.
    #[test]
    fn pass2_layer_priority_and_landuse() {
        use crate::proto::{Way, WayGroup};
        let block = WayBlock {
            stringtable: Some(StringTable {
                s: vec![
                    b"".to_vec(),
                    b"natural".to_vec(),
                    b"wood".to_vec(),
                    b"highway".to_vec(),
                    b"track".to_vec(),
                    b"landuse".to_vec(),
                    b"meadow".to_vec(),
                ],
            }),
            primitivegroup: vec![WayGroup {
                ways: vec![
                    // natural=wood AND highway=track → highway (priority 0).
                    Way {
                        id: 1,
                        keys: vec![1, 3],
                        vals: vec![2, 4],
                        refs: vec![10, 1],
                    },
                    // landuse=meadow → kept as layer 3.
                    Way {
                        id: 2,
                        keys: vec![5],
                        vals: vec![6],
                        refs: vec![20, 1],
                    },
                ],
            }],
        };
        let mut out = Vec::new();
        assert_eq!(extract_relevant_ways(&block, &mut out).unwrap(), 2);
        assert_eq!((out[0].layer, out[0].class.as_slice()), (0, &b"track"[..]));
        assert_eq!((out[1].layer, out[1].class.as_slice()), (3, &b"meadow"[..]));
    }

    #[test]
    fn pass2_yields_and_resumes_without_duplicates() {
        let dir = tmp_dir("pass2-yield");
        let (pbf, db) = indexed_fixture(&dir);

        let mut offset = 0usize;
        let mut total_ways = 0u64;
        let mut slices = 0u32;
        loop {
            let s = run_pass2_slice(&pbf, &db, offset, &mut || true).unwrap();
            assert!(
                s.blocks_scanned == 1 || (s.finished && s.blocks_scanned == 0),
                "slice made no progress: {s:?}"
            );
            total_ways += s.ways_indexed;
            offset = s.next_offset;
            slices += 1;
            if s.finished {
                break;
            }
            assert!(slices < 100);
        }
        assert_eq!(
            total_ways, 2,
            "per-slice sums == distinct ways (no re-scan)"
        );
        assert_eq!(get_way_refs(&db, 500).unwrap().unwrap().len(), 3);
    }

    #[test]
    fn assemble_way_geometry_joins_both_tables() {
        let dir = tmp_dir("assemble");
        let (pbf, db) = indexed_fixture(&dir);
        run_pass2_slice(&pbf, &db, 0, &mut || false).unwrap();

        let line = assemble_way_geometry(&db, 500).unwrap().unwrap();
        let want: Vec<(f64, f64)> = [1_000i64, 1_005, 900]
            .iter()
            .map(|id| {
                let &(_, lat, lon) = GROUP_A.iter().find(|(nid, _, _)| nid == id).unwrap();
                expected(lat, lon)
            })
            .collect();
        assert_eq!(line, want, "join must preserve ref order and projection");

        // Unknown way → None, not an error.
        assert_eq!(assemble_way_geometry(&db, 999).unwrap(), None);
    }

    #[test]
    fn assemble_handles_missing_nodes_safely() {
        let dir = tmp_dir("assemble-missing");
        let (_pbf, db) = indexed_fixture(&dir);

        // Ways with refs partially outside the extract (nodes never indexed)
        // — the clipped-extract reality at bbox edges.
        let hw = |id: u64, refs: Vec<u64>| IndexedWay {
            id,
            layer: 0,
            class: b"path".to_vec(),
            sac_scale: Vec::new(),
            name: Vec::new(),
            refs,
        };
        insert_ways_batched(
            &db,
            [
                hw(700, vec![1_000, 77_777, 1_005]), // 1 missing mid-way
                hw(701, vec![88_888, 99_999]),       // all missing
                hw(702, vec![1_000, 55_555]),        // only 1 resolvable
            ],
            DEFAULT_BATCH_SIZE,
        )
        .unwrap();

        let line = assemble_way_geometry(&db, 700).unwrap().unwrap();
        assert_eq!(line.len(), 2, "missing vertex skipped, not fatal");
        assert_eq!(
            line,
            vec![
                expected(472_700_000, 113_900_000),
                expected(472_700_100, 113_900_050)
            ]
        );

        assert_eq!(
            assemble_way_geometry(&db, 701).unwrap(),
            None,
            "zero resolvable vertices → no geometry"
        );
        assert_eq!(
            assemble_way_geometry(&db, 702).unwrap(),
            None,
            "a single vertex cannot form a linestring"
        );
    }

    #[test]
    fn corrupted_ways_rejected() {
        // Key index pointing outside the StringTable.
        let mut bad = WayBlock {
            stringtable: Some(StringTable {
                s: vec![b"".to_vec(), b"highway".to_vec(), b"path".to_vec()],
            }),
            primitivegroup: vec![crate::proto::WayGroup {
                ways: vec![crate::proto::Way {
                    id: 1,
                    keys: vec![9],
                    vals: vec![2],
                    refs: vec![10, 1],
                }],
            }],
        };
        let mut out = Vec::new();
        assert!(matches!(
            extract_relevant_ways(&bad, &mut out),
            Err(IndexError::InvalidInput(_))
        ));

        // keys/vals parallel-array length mismatch.
        bad.primitivegroup[0].ways[0].keys = vec![1];
        bad.primitivegroup[0].ways[0].vals = vec![];
        assert!(matches!(
            extract_relevant_ways(&bad, &mut out),
            Err(IndexError::InvalidInput(_))
        ));

        // Value index pointing outside the StringTable (on a layer key,
        // so the value actually gets resolved).
        bad.primitivegroup[0].ways[0].vals = vec![9];
        assert!(matches!(
            extract_relevant_ways(&bad, &mut out),
            Err(IndexError::InvalidInput(_))
        ));

        // Negative ref after delta accumulation.
        bad.primitivegroup[0].ways[0].vals = vec![2];
        bad.primitivegroup[0].ways[0].refs = vec![10, -100];
        assert!(matches!(
            extract_relevant_ways(&bad, &mut out),
            Err(IndexError::InvalidInput(_))
        ));

        // Negative way id.
        bad.primitivegroup[0].ways[0].id = -5;
        bad.primitivegroup[0].ways[0].refs = vec![10, 1];
        assert!(matches!(
            extract_relevant_ways(&bad, &mut out),
            Err(IndexError::InvalidInput(_))
        ));

        // Non-UTF-8 tag value on a layer key.
        bad.primitivegroup[0].ways[0].id = 7;
        bad.stringtable.as_mut().unwrap().s[2] = vec![0xff, 0xfe];
        assert!(matches!(
            extract_relevant_ways(&bad, &mut out),
            Err(IndexError::InvalidInput(_))
        ));
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

    #[test]
    fn pass1_extracts_peak_pois_behind_the_probe() {
        let dir = tmp_dir("pois");
        let pbf_path = dir.join("peaks.osm.pbf");
        std::fs::write(
            &pbf_path,
            crate::fixtures::synthetic_pbf_with_ways_and_peaks(
                &[&[(10, 472_700_000, 113_900_000)]],
                &[],
                &[
                    (500_000, 472_756_000, 113_910_000, b"Hafelekarspitze"),
                    (500_001, 472_800_000, 113_950_000, b""),
                ],
            ),
        )
        .unwrap();
        let pbf = PbfMmap::open(&pbf_path).unwrap();
        let db = crate::open_coord_db(&dir.join("idx.redb")).unwrap();

        let sub = run_pass1_slice(&pbf, &db, 0, &mut || false).unwrap();
        assert!(sub.finished);
        // The plain node AND both peak nodes land in COORDINATES (peaks are
        // ordinary nodes first, POIs second).
        assert_eq!(crate::coord_count(&db).unwrap(), 3);

        let pois = crate::all_pois(&db).unwrap();
        assert_eq!(pois.len(), 2);
        assert_eq!(pois[0].0, 500_000);
        assert_eq!(pois[0].3, b"Hafelekarspitze".to_vec());
        assert_eq!(
            pois[1].3,
            Vec::<u8>::new(),
            "unnamed summit still extracted"
        );
        let want = crate::web_mercator(
            1e-9 * (100 * 113_910_000i64) as f64,
            1e-9 * (100 * 472_756_000i64) as f64,
        );
        assert_eq!(
            (pois[0].1, pois[0].2),
            want,
            "POI coords use the block projection"
        );

        // Idempotent re-scan (the crash-resume path): same two rows.
        let again = run_pass1_slice(&pbf, &db, 0, &mut || false).unwrap();
        assert!(again.finished);
        assert_eq!(crate::poi_count(&db).unwrap(), 2);
    }

    #[test]
    fn peak_probe_rejects_blocks_without_the_strings() {
        let plain = crate::fixtures::dense_block(&[(1, 0, 0)], None);
        assert!(
            !block_mentions_peak(&plain),
            "peak-free block must skip keys_vals"
        );
        let peaks = crate::fixtures::peak_dense_block(&[(2, 0, 0, b"X")]);
        assert!(block_mentions_peak(&peaks));
    }
}
