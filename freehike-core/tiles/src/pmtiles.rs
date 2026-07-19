// SPDX-License-Identifier: Apache-2.0
//! PMTiles v3 wire encoding: the 127-byte header and varint directories.
//!
//! Pure byte-level building blocks — no I/O, no database. The archive
//! assembly that uses them lives in [`crate::finalize`].
//!
//! Layout produced by this writer (spec section order):
//! `header (127) | root directory | JSON metadata | leaf directories | tile data`
//! P5.C1 writes a **root-only** directory (leaf section present but empty)
//! — spec-valid at any entry count, though the spec *recommends* leaf
//! splitting once the root outgrows the initial-fetch window; that split is
//! a logged follow-up chunk.

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

/// One directory entry. `offset`/`length` address the tile-data section
/// (offset 0 = first data byte); `run_length` ≥ 1 means this payload serves
/// that many consecutive tile IDs (P5.C1 always writes 1 — run coalescing
/// is a logged follow-up optimization).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirEntry {
    pub tile_id: u64,
    pub offset: u64,
    pub length: u32,
    pub run_length: u32,
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

    #[test]
    fn gzip_roundtrips_and_is_deterministic() {
        let data = b"the same bytes must gzip to the same bytes".repeat(20);
        let a = gzip(&data);
        let b = gzip(&data);
        assert_eq!(a, b, "gzip must be deterministic for dedup");
        assert_eq!(gunzip(&a), data);
    }
}
