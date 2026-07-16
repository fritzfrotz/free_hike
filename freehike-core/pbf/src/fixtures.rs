//! Synthetic-but-wire-valid PBF builders.
//!
//! Shared by this crate's own tests and, via the `fixtures` cargo feature, by
//! downstream crates' test suites (`compiler`, `ffi`) that need a real file
//! for the integrated Pass-1 engine. Never part of a production build: the
//! module only compiles under `cfg(test)` or the explicitly-enabled feature.

use prost::Message;

use crate::proto::{
    Blob, BlobHeader, DenseNodes, PrimitiveBlock, PrimitiveGroup, StringTable, Way, WayBlock,
    WayGroup,
};

/// Frames one `[u32 BE len][BlobHeader][Blob]` record.
pub fn frame(blob_type: &str, blob: &Blob) -> Vec<u8> {
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
pub fn data_blob(block: &PrimitiveBlock) -> Blob {
    let payload = block.encode_to_vec();
    Blob {
        raw: None,
        raw_size: Some(payload.len() as i32),
        zlib_data: Some(miniz_oxide::deflate::compress_to_vec_zlib(&payload, 6)),
    }
}

/// Delta-encodes absolute `(id, lat_units, lon_units)` triples into a
/// DenseNodes-bearing PrimitiveBlock — the inverse of Pass 1's decoding.
/// Units are granularity units (default granularity: 1 unit = 1e-7 deg).
pub fn dense_block(nodes: &[(i64, i64, i64)], granularity: Option<i32>) -> PrimitiveBlock {
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

/// A complete synthetic PBF byte stream: one OSMHeader blob followed by one
/// zlib OSMData blob per node group.
pub fn synthetic_pbf(groups: &[&[(i64, i64, i64)]]) -> Vec<u8> {
    synthetic_pbf_with_ways(groups, &[])
}

/// One way for [`way_block`] / [`synthetic_pbf_with_ways`]:
/// `(way_id, tag_key, tag_value, absolute node refs)`.
pub type FixtureWay<'a> = (i64, &'a [u8], &'a [u8], &'a [i64]);

/// Builds a way-bearing OSMData block: a StringTable holding each distinct
/// tag key and value (index 0 reserved empty, per OSM convention), and one
/// Way per entry with parallel `keys`/`vals` and delta-encoded refs — the
/// inverse of Pass 2's decoding.
pub fn way_block(ways: &[FixtureWay<'_>]) -> Vec<u8> {
    let mut strings: Vec<Vec<u8>> = vec![Vec::new()]; // index 0: empty
    let intern = |s: &[u8], strings: &mut Vec<Vec<u8>>| -> u32 {
        match strings.iter().position(|x| x == s) {
            Some(i) => i as u32,
            None => {
                strings.push(s.to_vec());
                (strings.len() - 1) as u32
            }
        }
    };
    let mut way_msgs = Vec::with_capacity(ways.len());
    for &(id, key, value, refs) in ways {
        let key_idx = intern(key, &mut strings);
        let val_idx = intern(value, &mut strings);
        let mut deltas = Vec::with_capacity(refs.len());
        let mut prev = 0i64;
        for &r in refs {
            deltas.push(r - prev);
            prev = r;
        }
        way_msgs.push(Way {
            id,
            keys: vec![key_idx],
            vals: vec![val_idx],
            refs: deltas,
        });
    }
    let block = WayBlock {
        stringtable: Some(StringTable { s: strings }),
        primitivegroup: vec![WayGroup { ways: way_msgs }],
    };
    // WayBlock encodes to the same wire tags as a way-bearing PrimitiveBlock.
    let payload = block.encode_to_vec();
    frame(
        "OSMData",
        &Blob {
            raw: None,
            raw_size: Some(payload.len() as i32),
            zlib_data: Some(miniz_oxide::deflate::compress_to_vec_zlib(&payload, 6)),
        },
    )
}

/// Full fixture: OSMHeader, then node blocks, then way blocks — the same
/// nodes-before-ways ordering real planet extracts use.
pub fn synthetic_pbf_with_ways(
    node_groups: &[&[(i64, i64, i64)]],
    way_groups: &[&[FixtureWay<'_>]],
) -> Vec<u8> {
    let mut bytes = frame(
        "OSMHeader",
        &Blob {
            raw: Some(b"stub header block".to_vec()),
            raw_size: None,
            zlib_data: None,
        },
    );
    for group in node_groups {
        bytes.extend_from_slice(&frame("OSMData", &data_blob(&dense_block(group, None))));
    }
    for ways in way_groups {
        bytes.extend_from_slice(&way_block(ways));
    }
    bytes
}
