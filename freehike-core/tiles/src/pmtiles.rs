// SPDX-License-Identifier: Apache-2.0
//! PMTiles v3 wire encoding: the 127-byte header and varint directories.
//!
//! Pure byte-level building blocks — no I/O, no database. The archive
//! assembly that uses them lives in [`crate::finalize`].
//!
//! Layout produced by this writer (spec section order):
//! `header (127) | root directory | JSON metadata | leaf directories | tile data`
//!
//! P-CORE.C7 (closes D001): the writer now emits the full directory model —
//! consecutive tile IDs sharing one payload coalesce into `run_length > 1`
//! entries ([`coalesce_runs`]), and once the serialized root outgrows
//! [`ROOT_DIR_BUDGET_BYTES`] it splits into individually-gzip'd leaf
//! directories addressed by `run_length == 0` pointer entries in the root
//! ([`build_directories`]). Small archives keep the root-only layout
//! byte-for-byte (leaf section present but empty).

use std::io::Write;

use flate2::write::GzEncoder;
use flate2::Compression;

/// Exact header size fixed by the spec.
pub const HEADER_BYTES: usize = 127;

pub const MAGIC: &[u8; 7] = b"PMTiles";
pub const SPEC_VERSION: u8 = 3;

/// `compression` enum values (spec).
pub const COMPRESSION_NONE: u8 = 1;
pub const COMPRESSION_GZIP: u8 = 2;
/// `tile_type` enum values (spec).
pub const TILE_TYPE_MVT: u8 = 1;
pub const TILE_TYPE_WEBP: u8 = 4;

/// One directory entry. In a tile directory, `offset`/`length` address the
/// tile-data section (offset 0 = first data byte) and `run_length ≥ 1`
/// means this payload serves that many CONSECUTIVE tile IDs. In the root of
/// a split archive, `run_length == 0` marks a LEAF POINTER: `offset`/
/// `length` then address the leaf-directories section instead (spec §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirEntry {
    pub tile_id: u64,
    pub offset: u64,
    pub length: u32,
    pub run_length: u32,
}

/// Split threshold: the spec recommends the root directory stay within the
/// reader's initial fetch window (16 KiB). We compare the UNCOMPRESSED
/// serialized root against it — conservative (the gzip'd root is smaller),
/// deterministic, and spec-valid on both sides of the boundary.
pub const ROOT_DIR_BUDGET_BYTES: usize = 16_384;

/// First leaf-partition size tried by [`build_directories`]; doubled until
/// the root of leaf pointers fits the budget.
const LEAF_SIZE_START: usize = 4_096;

/// Collapses consecutive entries that address the SAME payload for
/// CONSECUTIVE tile IDs into one `run_length > 1` entry. Input must be
/// sorted by tile ID (redb key order guarantees this upstream); dedup
/// back-references coalesce naturally because they share offset+length.
pub fn coalesce_runs(entries: Vec<DirEntry>) -> Vec<DirEntry> {
    let mut out: Vec<DirEntry> = Vec::with_capacity(entries.len());
    for e in entries {
        if let Some(p) = out.last_mut() {
            if e.tile_id == p.tile_id + u64::from(p.run_length)
                && e.offset == p.offset
                && e.length == p.length
            {
                p.run_length += 1;
                continue;
            }
        }
        out.push(e);
    }
    out
}

/// Builds the directory sections for an archive: returns
/// `(gzip'd root, concatenated gzip'd leaves)`. Under the budget the root
/// holds the entries directly and the leaf section is empty; over it, the
/// entries are partitioned into leaves (each serialized + gzip'd on its
/// own, so a reader fetches exactly one leaf per lookup) and the root
/// holds one `run_length == 0` pointer per leaf, keyed by the leaf's first
/// tile ID. Leaf size starts at [`LEAF_SIZE_START`] entries and doubles
/// until the root itself fits the budget.
pub fn build_directories(entries: &[DirEntry]) -> (Vec<u8>, Vec<u8>) {
    let root_plain = serialize_directory(entries);
    if root_plain.len() <= ROOT_DIR_BUDGET_BYTES {
        return (gzip(&root_plain), Vec::new());
    }

    let mut leaf_size = LEAF_SIZE_START;
    loop {
        let mut leaves = Vec::new();
        let mut root_entries: Vec<DirEntry> = Vec::with_capacity(entries.len() / leaf_size + 1);
        for chunk in entries.chunks(leaf_size) {
            let leaf_gz = gzip(&serialize_directory(chunk));
            root_entries.push(DirEntry {
                tile_id: chunk[0].tile_id,
                offset: leaves.len() as u64, // relative to the leaf section
                length: leaf_gz.len() as u32,
                run_length: 0, // leaf pointer
            });
            leaves.extend_from_slice(&leaf_gz);
        }
        let root_plain = serialize_directory(&root_entries);
        if root_plain.len() <= ROOT_DIR_BUDGET_BYTES || leaf_size >= entries.len() {
            return (gzip(&root_plain), leaves);
        }
        leaf_size *= 2;
    }
}

/// Section offsets and stats the header serializes. All offsets are
/// absolute file positions; lengths are byte counts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Header {
    pub root_dir_offset: u64,
    pub root_dir_length: u64,
    pub metadata_offset: u64,
    pub metadata_length: u64,
    pub leaf_dirs_offset: u64,
    pub leaf_dirs_length: u64,
    pub tile_data_offset: u64,
    pub tile_data_length: u64,
    pub n_addressed_tiles: u64,
    pub n_tile_entries: u64,
    pub n_tile_contents: u64,
    pub clustered: bool,
    /// Payload compression declared at byte 98 (P5 vector: gzip'd MVT;
    /// P6 terrain: none — WebP is already entropy-coded and lossless-exact).
    pub tile_compression: u8,
    /// Payload format declared at byte 99 (`TILE_TYPE_*`).
    pub tile_type: u8,
    pub min_zoom: u8,
    pub max_zoom: u8,
    /// (west, south, east, north) in degrees.
    pub bounds_deg: (f64, f64, f64, f64),
    pub center_zoom: u8,
    /// (lon, lat) in degrees.
    pub center_deg: (f64, f64),
}

fn e7(deg: f64) -> i32 {
    (deg * 10_000_000.0).round() as i32
}

/// Serializes the fixed 127-byte header (all integers little-endian, field
/// offsets per spec).
pub fn encode_header(h: &Header) -> [u8; HEADER_BYTES] {
    let mut out = [0u8; HEADER_BYTES];
    out[0..7].copy_from_slice(MAGIC);
    out[7] = SPEC_VERSION;

    let put_u64 = |buf: &mut [u8; HEADER_BYTES], at: usize, v: u64| {
        buf[at..at + 8].copy_from_slice(&v.to_le_bytes());
    };
    put_u64(&mut out, 8, h.root_dir_offset);
    put_u64(&mut out, 16, h.root_dir_length);
    put_u64(&mut out, 24, h.metadata_offset);
    put_u64(&mut out, 32, h.metadata_length);
    put_u64(&mut out, 40, h.leaf_dirs_offset);
    put_u64(&mut out, 48, h.leaf_dirs_length);
    put_u64(&mut out, 56, h.tile_data_offset);
    put_u64(&mut out, 64, h.tile_data_length);
    put_u64(&mut out, 72, h.n_addressed_tiles);
    put_u64(&mut out, 80, h.n_tile_entries);
    put_u64(&mut out, 88, h.n_tile_contents);

    out[96] = u8::from(h.clustered);
    out[97] = COMPRESSION_GZIP; // internal (directory/metadata) compression
    out[98] = h.tile_compression;
    out[99] = h.tile_type;
    out[100] = h.min_zoom;
    out[101] = h.max_zoom;

    let (west, south, east, north) = h.bounds_deg;
    out[102..106].copy_from_slice(&e7(west).to_le_bytes());
    out[106..110].copy_from_slice(&e7(south).to_le_bytes());
    out[110..114].copy_from_slice(&e7(east).to_le_bytes());
    out[114..118].copy_from_slice(&e7(north).to_le_bytes());
    out[118] = h.center_zoom;
    out[119..123].copy_from_slice(&e7(h.center_deg.0).to_le_bytes());
    out[123..127].copy_from_slice(&e7(h.center_deg.1).to_le_bytes());
    out
}

fn push_varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            break;
        }
        out.push(byte | 0x80);
    }
}

/// Serializes a directory (spec: entry count, then per-column varints —
/// tile-ID deltas, run lengths, lengths, offsets). Entries MUST be sorted
/// by ascending `tile_id`; the offset column uses the spec's `0 =
/// contiguous with previous entry` compression, else `offset + 1`.
///
/// The result is the *uncompressed* directory; callers gzip it (internal
/// compression) before writing.
pub fn serialize_directory(entries: &[DirEntry]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + entries.len() * 6);
    push_varint(&mut out, entries.len() as u64);

    let mut last_id = 0u64;
    for e in entries {
        debug_assert!(e.tile_id >= last_id, "directory entries must be sorted");
        push_varint(&mut out, e.tile_id - last_id);
        last_id = e.tile_id;
    }
    for e in entries {
        push_varint(&mut out, u64::from(e.run_length));
    }
    for e in entries {
        push_varint(&mut out, u64::from(e.length));
    }
    let mut prev: Option<&DirEntry> = None;
    for e in entries {
        match prev {
            Some(p) if e.offset == p.offset + u64::from(p.length) => push_varint(&mut out, 0),
            _ => push_varint(&mut out, e.offset + 1),
        }
        prev = Some(e);
    }
    out
}

/// Gzip with the crate's fixed settings (default level, zero mtime) —
/// deterministic for identical input, which the payload dedup and the
/// engine's sliced-equals-single determinism proof both rely on.
pub fn gzip(data: &[u8]) -> Vec<u8> {
    let mut enc = GzEncoder::new(
        Vec::with_capacity(data.len() / 2 + 32),
        Compression::default(),
    );
    // Writing to a Vec cannot fail.
    enc.write_all(data).expect("gzip to Vec");
    enc.finish().expect("gzip finish")
}

#[cfg(test)]
pub(crate) fn gunzip(data: &[u8]) -> Vec<u8> {
    use std::io::Read;
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(data)
        .read_to_end(&mut out)
        .expect("gunzip");
    out
}

/// Test-side directory parser (inverse of [`serialize_directory`]). Kept
/// crate-internal until a reader ships; the integration tests use it to
/// prove the writer against the spec rather than against itself... which it
/// still partially is — the byte-level golden test below is the anchor.
#[cfg(test)]
pub(crate) fn parse_directory(bytes: &[u8]) -> Vec<DirEntry> {
    fn read_varint(bytes: &[u8], pos: &mut usize) -> u64 {
        let mut v = 0u64;
        let mut shift = 0;
        loop {
            let b = bytes[*pos];
            *pos += 1;
            v |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return v;
            }
            shift += 7;
        }
    }

    let mut pos = 0usize;
    let n = read_varint(bytes, &mut pos) as usize;
    let mut ids = Vec::with_capacity(n);
    let mut acc = 0u64;
    for _ in 0..n {
        acc += read_varint(bytes, &mut pos);
        ids.push(acc);
    }
    let runs: Vec<u64> = (0..n).map(|_| read_varint(bytes, &mut pos)).collect();
    let lens: Vec<u64> = (0..n).map(|_| read_varint(bytes, &mut pos)).collect();

    let mut entries: Vec<DirEntry> = Vec::with_capacity(n);
    for i in 0..n {
        let raw = read_varint(bytes, &mut pos);
        let offset = if raw == 0 {
            let p = &entries[i - 1];
            p.offset + u64::from(p.length)
        } else {
            raw - 1
        };
        entries.push(DirEntry {
            tile_id: ids[i],
            offset,
            length: lens[i] as u32,
            run_length: runs[i] as u32,
        });
    }
    assert_eq!(pos, bytes.len(), "trailing directory bytes");
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_is_byte_exact() {
        let h = Header {
            root_dir_offset: 127,
            root_dir_length: 25,
            metadata_offset: 152,
            metadata_length: 10,
            leaf_dirs_offset: 162,
            leaf_dirs_length: 0,
            tile_data_offset: 162,
            tile_data_length: 4000,
            n_addressed_tiles: 3,
            n_tile_entries: 3,
            n_tile_contents: 2,
            clustered: true,
            tile_compression: COMPRESSION_GZIP,
            tile_type: TILE_TYPE_MVT,
            min_zoom: 14,
            max_zoom: 14,
            bounds_deg: (11.15, 47.05, 11.65, 47.45),
            center_zoom: 14,
            center_deg: (11.4, 47.25),
        };
        let bytes = encode_header(&h);
        assert_eq!(bytes.len(), HEADER_BYTES);
        assert_eq!(&bytes[0..7], MAGIC);
        assert_eq!(bytes[7], 3);
        assert_eq!(u64::from_le_bytes(bytes[8..16].try_into().unwrap()), 127);
        assert_eq!(u64::from_le_bytes(bytes[56..64].try_into().unwrap()), 162);
        assert_eq!(u64::from_le_bytes(bytes[64..72].try_into().unwrap()), 4000);
        assert_eq!(u64::from_le_bytes(bytes[88..96].try_into().unwrap()), 2);
        assert_eq!(bytes[96], 1);
        assert_eq!(bytes[97], COMPRESSION_GZIP);
        assert_eq!(bytes[98], COMPRESSION_GZIP);
        assert_eq!(bytes[99], TILE_TYPE_MVT);
        assert_eq!(bytes[100], 14);
        assert_eq!(bytes[101], 14);
        assert_eq!(
            i32::from_le_bytes(bytes[102..106].try_into().unwrap()),
            111_500_000
        );
        assert_eq!(
            i32::from_le_bytes(bytes[114..118].try_into().unwrap()),
            474_500_000
        );
        assert_eq!(
            i32::from_le_bytes(bytes[123..127].try_into().unwrap()),
            472_500_000
        );
    }

    /// Golden bytes for a tiny directory: pins the varint column layout and
    /// the offset-continuation encoding against the spec by hand, not
    /// against our own parser.
    #[test]
    fn directory_golden_bytes() {
        let entries = [
            DirEntry {
                tile_id: 100,
                offset: 0,
                length: 10,
                run_length: 1,
            },
            DirEntry {
                tile_id: 130,
                offset: 10,
                length: 200,
                run_length: 1,
            },
            // Dedup back-reference: NOT contiguous with entry 2.
            DirEntry {
                tile_id: 131,
                offset: 0,
                length: 10,
                run_length: 1,
            },
        ];
        let bytes = serialize_directory(&entries);
        assert_eq!(
            bytes,
            vec![
                3, // n entries
                100, 30, 1, // id deltas
                1, 1, 1, // run lengths
                10, 0xC8, 0x01, 10, // lengths (200 = 0xC8 0x01 varint)
                1,  // offset 0 → 0+1
                0,  // contiguous with previous (0+10)
                1,  // back-reference to offset 0 → 0+1
            ]
        );
    }

    #[test]
    fn directory_roundtrips_through_parser() {
        let entries: Vec<DirEntry> = (0..500u64)
            .map(|i| DirEntry {
                tile_id: 89_478_485 + i * 3,
                offset: i * 100,
                length: 100,
                run_length: 1,
            })
            .collect();
        let parsed = parse_directory(&serialize_directory(&entries));
        assert_eq!(parsed, entries);
    }

    fn entry(tile_id: u64, offset: u64, length: u32, run_length: u32) -> DirEntry {
        DirEntry {
            tile_id,
            offset,
            length,
            run_length,
        }
    }

    #[test]
    fn coalesce_collapses_consecutive_identical_payloads() {
        let coalesced = coalesce_runs(vec![
            entry(100, 0, 10, 1),
            entry(101, 0, 10, 1),  // run with 100
            entry(102, 0, 10, 1),  // run with 100
            entry(103, 20, 10, 1), // same ids run but different payload
            entry(105, 0, 10, 1),  // same payload but id gap
        ]);
        assert_eq!(
            coalesced,
            vec![
                entry(100, 0, 10, 3),
                entry(103, 20, 10, 1),
                entry(105, 0, 10, 1)
            ]
        );
        // Addressed tiles are preserved through the collapse.
        let addressed: u64 = coalesced.iter().map(|e| u64::from(e.run_length)).sum();
        assert_eq!(addressed, 5);
    }

    #[test]
    fn coalesce_is_identity_for_runless_input() {
        let entries = vec![entry(10, 0, 5, 1), entry(12, 5, 6, 1), entry(20, 11, 7, 1)];
        assert_eq!(coalesce_runs(entries.clone()), entries);
    }

    #[test]
    fn build_directories_keeps_small_roots_leafless() {
        let entries: Vec<DirEntry> = (0..100u64).map(|i| entry(i * 2, i * 10, 10, 1)).collect();
        let (root_gz, leaves) = build_directories(&entries);
        assert!(leaves.is_empty());
        assert_eq!(parse_directory(&gunzip(&root_gz)), entries);
    }

    #[test]
    fn build_directories_splits_oversized_roots_into_resolvable_leaves() {
        // IDs spaced by 2 so nothing coalesces; ~30k entries serialize far
        // past the 16 KiB root budget.
        let entries: Vec<DirEntry> = (0..30_000u64)
            .map(|i| entry(i * 2, (i % 7) * 100, 100, 1))
            .collect();
        let (root_gz, leaves) = build_directories(&entries);
        assert!(!leaves.is_empty(), "must split");

        let root = parse_directory(&gunzip(&root_gz));
        assert!(
            serialize_directory(&root).len() <= ROOT_DIR_BUDGET_BYTES,
            "root of leaf pointers must fit the budget"
        );
        assert!(
            root.iter().all(|e| e.run_length == 0),
            "root holds leaf pointers only"
        );
        assert!(
            root.len() < entries.len() / 100,
            "root is a small index over leaves"
        );

        // Resolve a probe tile through its leaf, spec-style: last root entry
        // with tile_id <= probe → gunzip that leaf slice → find the entry.
        let probe = entries[12_345];
        let leaf_ptr = root
            .iter()
            .rev()
            .find(|e| e.tile_id <= probe.tile_id)
            .expect("a leaf must cover the probe");
        let start = leaf_ptr.offset as usize;
        let leaf = parse_directory(&gunzip(&leaves[start..start + leaf_ptr.length as usize]));
        assert_eq!(
            leaf.iter().find(|e| e.tile_id == probe.tile_id),
            Some(&probe),
            "leaf lookup must return the original entry byte-exactly"
        );

        // Every entry survives the partition, in order.
        let mut reassembled = Vec::with_capacity(entries.len());
        for ptr in &root {
            let s = ptr.offset as usize;
            reassembled.extend(parse_directory(&gunzip(
                &leaves[s..s + ptr.length as usize],
            )));
        }
        assert_eq!(reassembled, entries);

        // Determinism (the engine's sliced-equals-single proof rides on it).
        let (root2, leaves2) = build_directories(&entries);
        assert_eq!((root_gz, leaves), (root2, leaves2));
    }

    #[test]
    fn gzip_roundtrips_and_is_deterministic() {
        let data = b"the same bytes must gzip to the same bytes".repeat(20);
        let a = gzip(&data);
        let b = gzip(&data);
        assert_eq!(a, b, "gzip must be deterministic for dedup");
        assert_eq!(gunzip(&a), data);
    }
}
