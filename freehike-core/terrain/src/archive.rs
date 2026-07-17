//! `terrain.pmtiles` assembly: pyramid enumeration → rendered WebP tiles →
//! PMTiles v3 archive through the Phase-5 byte-level writer.
//!
//! The append-only posture carries over from the vector pipeline: tile
//! coordinates for the whole z-range are enumerated up front, sorted by
//! Hilbert tile ID, and rendered IN that order, so payload bytes stream
//! sequentially into the data section exactly once (`clustered = 1`, no
//! shuffle pass). Memory holds the directory entries (16B × tile count) and
//! one tile in flight — the sampler's bounded chunk cache does the rest.
//!
//! Unlike the P5 vector archive, tile payloads are NOT gzipped: WebP is
//! already entropy-coded, so the header declares `tile_compression = none`,
//! `tile_type = webp`. Internal compression (directories/metadata) stays
//! gzip per the shared writer.

use std::fs::File;
use std::io::{BufWriter, Read, Seek, Write};
use std::path::{Path, PathBuf};

use tiles::hilbert::tile_id;
use tiles::pmtiles::{
    encode_header, gzip, serialize_directory, DirEntry, Header, COMPRESSION_NONE, HEADER_BYTES,
    TILE_TYPE_WEBP,
};

use crate::mercator::{tile_range_for_bounds, TileCoord};
use crate::pyramid::render_tile;
use crate::reader::DemError;
use crate::sample::DemSampler;
use crate::TerrainError;

/// Default pyramid range (ARCHITECTURE.md §4: massif params, z5–12).
pub const MIN_ZOOM: u8 = 5;
pub const MAX_ZOOM: u8 = 12;

/// What was written, for logging and assertions.
#[derive(Debug)]
pub struct ArchiveReport {
    pub path: PathBuf,
    pub tile_count: u64,
    pub tile_data_bytes: u64,
    pub archive_bytes: u64,
    pub bounds_deg: (f64, f64, f64, f64),
}

/// Builds `terrain.pmtiles` at `out_path` covering the DEM's full extent for
/// zooms `min_zoom..=max_zoom`. Atomic: written beside the target and
/// renamed into place.
pub fn build_terrain_archive<R: Read + Seek>(
    sampler: &mut DemSampler<R>,
    out_path: &Path,
    min_zoom: u8,
    max_zoom: u8,
) -> Result<ArchiveReport, TerrainError> {
    assert!(min_zoom <= max_zoom, "inverted zoom range");
    let bounds = sampler
        .reader()
        .geo_bounds()
        .ok_or(DemError::MissingGeoTransform)?;

    // Enumerate the whole pyramid and sort by Hilbert tile ID — ascending
    // IDs order z5 before z12 for free (the ID space is zoom-prefixed).
    let coords = tile_id_range_sorted(bounds, min_zoom, max_zoom);

    // Render in ID order, streaming payloads to a data temp file so peak
    // memory is one tile regardless of pyramid size.
    let data_tmp = out_path.with_extension("data.tmp");
    let mut dir: Vec<DirEntry> = Vec::with_capacity(coords.len());
    let mut data_len = 0u64;
    {
        let mut data = BufWriter::new(File::create(&data_tmp)?);
        for &(id, coord) in &coords {
            let tile = render_tile(sampler, coord)?;
            data.write_all(&tile.webp)?;
            dir.push(DirEntry {
                tile_id: id,
                offset: data_len,
                length: tile.webp.len() as u32,
                run_length: 1,
            });
            data_len += tile.webp.len() as u64;
        }
        data.flush()?;
        data.get_ref().sync_all()?;
    }

    let root_dir = gzip(&serialize_directory(&dir));
    let metadata = gzip(terrain_metadata_json(bounds, min_zoom, max_zoom).as_bytes());

    let root_dir_offset = HEADER_BYTES as u64;
    let metadata_offset = root_dir_offset + root_dir.len() as u64;
    let leaf_dirs_offset = metadata_offset + metadata.len() as u64;
    let (west, south, east, north) = bounds;
    let header = Header {
        root_dir_offset,
        root_dir_length: root_dir.len() as u64,
        metadata_offset,
        metadata_length: metadata.len() as u64,
        leaf_dirs_offset,
        leaf_dirs_length: 0, // root-only directory, as in P5
        tile_data_offset: leaf_dirs_offset,
        tile_data_length: data_len,
        n_addressed_tiles: dir.len() as u64,
        n_tile_entries: dir.len() as u64,
        n_tile_contents: dir.len() as u64, // no dedup: every payload unique
        clustered: true,
        tile_compression: COMPRESSION_NONE,
        tile_type: TILE_TYPE_WEBP,
        min_zoom,
        max_zoom,
        bounds_deg: bounds,
        center_zoom: min_zoom,
        center_deg: ((west + east) / 2.0, (south + north) / 2.0),
    };

    // Atomic final write: header + directory + metadata, then stream-copy
    // the data section, fsync, rename.
    let archive_tmp = out_path.with_extension("pmtiles.tmp");
    let mut out = BufWriter::new(File::create(&archive_tmp)?);
    out.write_all(&encode_header(&header))?;
    out.write_all(&root_dir)?;
    out.write_all(&metadata)?;
    let mut data = File::open(&data_tmp)?;
    std::io::copy(&mut data, &mut out)?;
    out.flush()?;
    out.get_ref().sync_all()?;
    drop(out);
    std::fs::rename(&archive_tmp, out_path)?;
    let _ = std::fs::remove_file(&data_tmp);

    Ok(ArchiveReport {
        path: out_path.to_path_buf(),
        tile_count: dir.len() as u64,
        tile_data_bytes: data_len,
        archive_bytes: leaf_dirs_offset + data_len,
        bounds_deg: bounds,
    })
}

/// PMTiles metadata JSON for the terrain layer. `format`/`type` follow the
/// PMTiles metadata conventions; `encoding: "mapbox"` is what raster-dem
/// consumers (maplibre) read to pick the Terrain-RGB decode equation.
fn terrain_metadata_json(bounds: (f64, f64, f64, f64), min_zoom: u8, max_zoom: u8) -> String {
    let (west, south, east, north) = bounds;
    format!(
        concat!(
            r#"{{"name":"freehike-terrain","format":"webp","type":"baselayer","#,
            r#""encoding":"mapbox","minzoom":"{}","maxzoom":"{}","#,
            r#""bounds":"{:.6},{:.6},{:.6},{:.6}"}}"#
        ),
        min_zoom, max_zoom, west, south, east, north
    )
}

/// PMTiles tile ID (Hilbert) of a tile coordinate.
pub fn hilbert_id(coord: TileCoord) -> u64 {
    tile_id(coord.z, coord.x, coord.y)
}

/// Enumerates every tile intersecting `bounds` for `min_zoom..=max_zoom`
/// and returns `(tile_id, coord)` pairs sorted ascending by ID — the write
/// order the append-only data section requires. IDs are unique by
/// construction (each coordinate appears once), so the sort is total.
pub fn tile_id_range_sorted(
    bounds: (f64, f64, f64, f64),
    min_zoom: u8,
    max_zoom: u8,
) -> Vec<(u64, TileCoord)> {
    let mut coords: Vec<(u64, TileCoord)> = (min_zoom..=max_zoom)
        .flat_map(|z| {
            tile_range_for_bounds(bounds, z)
                .coords()
                .collect::<Vec<_>>()
        })
        .map(|c| (hilbert_id(c), c))
        .collect();
    coords.sort_unstable_by_key(|(id, _)| *id);
    coords
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader::{test_dem, WindowedDemReader};
    use crate::rgb::rgb_to_elevation;
    use crate::sample::DemSampler;

    /// Innsbruck-shaped synthetic DEM (see pyramid tests): lon
    /// 11.099861…11.699861, lat 47.050139…47.450139, ramp 500…1285m.
    fn sampler() -> DemSampler<std::io::Cursor<Vec<u8>>> {
        let dem = test_dem::build(60, 40, 9999, Some((11.099861, 47.450139, 0.01)), |x, y| {
            (500 + 10 * x + 5 * y) as u16
        });
        DemSampler::new(WindowedDemReader::new(dem).unwrap()).unwrap()
    }

    /// Minimal test-side PMTiles reader: header fields + root directory.
    struct ParsedArchive {
        header: Vec<u8>,
        entries: Vec<DirEntry>,
        metadata: String,
        tile_data: Vec<u8>,
    }

    fn parse_archive(bytes: &[u8]) -> ParsedArchive {
        let u64_at = |at: usize| u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap());
        let gunzip = |slice: &[u8]| {
            use std::io::Read;
            let mut out = Vec::new();
            flate2::read::GzDecoder::new(slice)
                .read_to_end(&mut out)
                .unwrap();
            out
        };
        let root = gunzip(&bytes[u64_at(8) as usize..(u64_at(8) + u64_at(16)) as usize]);
        let metadata = gunzip(&bytes[u64_at(24) as usize..(u64_at(24) + u64_at(32)) as usize]);
        ParsedArchive {
            header: bytes[..HEADER_BYTES].to_vec(),
            entries: parse_directory(&root),
            metadata: String::from_utf8(metadata).unwrap(),
            tile_data: bytes[u64_at(56) as usize..(u64_at(56) + u64_at(64)) as usize].to_vec(),
        }
    }

    /// Inverse of `tiles::pmtiles::serialize_directory` (that crate's parser
    /// is test-internal, so the spec decode is repeated here).
    fn parse_directory(bytes: &[u8]) -> Vec<DirEntry> {
        fn varint(bytes: &[u8], pos: &mut usize) -> u64 {
            let (mut v, mut shift) = (0u64, 0);
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
        let mut pos = 0;
        let n = varint(bytes, &mut pos) as usize;
        let mut ids = Vec::with_capacity(n);
        let mut acc = 0u64;
        for _ in 0..n {
            acc += varint(bytes, &mut pos);
            ids.push(acc);
        }
        let runs: Vec<u64> = (0..n).map(|_| varint(bytes, &mut pos)).collect();
        let lens: Vec<u64> = (0..n).map(|_| varint(bytes, &mut pos)).collect();
        let mut entries: Vec<DirEntry> = Vec::with_capacity(n);
        for i in 0..n {
            let raw = varint(bytes, &mut pos);
            let offset = if raw == 0 {
                entries[i - 1].offset + u64::from(entries[i - 1].length)
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
        entries
    }

    #[test]
    fn archive_is_hilbert_ordered_webp_and_spec_shaped() {
        let dir = std::env::temp_dir().join(format!("terrain-archive-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("terrain.pmtiles");

        // z5–7 keeps the render count small for L1 (the full z-range is the
        // L2 real-data test's job).
        let report = build_terrain_archive(&mut sampler(), &out, 5, 7).unwrap();
        let bytes = std::fs::read(&out).unwrap();
        let parsed = parse_archive(&bytes);

        // Header: magic, spec version, webp/no-compression declaration,
        // zoom range, e7 bounds matching the DEM extent.
        assert_eq!(&parsed.header[0..7], b"PMTiles");
        assert_eq!(parsed.header[7], 3);
        assert_eq!(parsed.header[96], 1, "clustered");
        assert_eq!(parsed.header[98], COMPRESSION_NONE);
        assert_eq!(parsed.header[99], TILE_TYPE_WEBP);
        assert_eq!((parsed.header[100], parsed.header[101]), (5, 7));
        let e7 = |at: usize| i32::from_le_bytes(parsed.header[at..at + 4].try_into().unwrap());
        assert_eq!(e7(102), 110_998_610); // west
        assert_eq!(e7(114), 474_501_390); // north

        // Directory: every enumerated tile present, strictly ascending IDs,
        // contiguous append-only offsets (clustered, no gaps).
        assert_eq!(report.tile_count, parsed.entries.len() as u64);
        let mut expected_next = 0u64;
        let mut last_id = None;
        for e in &parsed.entries {
            assert!(last_id < Some(e.tile_id), "IDs must strictly ascend");
            last_id = Some(e.tile_id);
            assert_eq!(e.offset, expected_next, "data must be append-only");
            expected_next += u64::from(e.length);
        }
        assert_eq!(expected_next, report.tile_data_bytes);

        // Every payload is a raw (uncompressed) 256×256 WebP; spot-decode
        // one and confirm it carries ramp elevations.
        for e in &parsed.entries {
            let p = &parsed.tile_data[e.offset as usize..(e.offset + u64::from(e.length)) as usize];
            assert_eq!(&p[0..4], b"RIFF");
            assert_eq!(&p[8..12], b"WEBP");
        }
        let first = &parsed.tile_data[..parsed.entries[0].length as usize];
        let decoded = image::load_from_memory(first).unwrap().into_rgb8();
        assert_eq!(decoded.dimensions(), (256, 256));
        assert!(decoded
            .pixels()
            .any(|px| (500.0..=1285.0).contains(&rgb_to_elevation(px.0))));

        // The IDs are exactly the Hilbert IDs of the enumerated pyramid.
        let coords = tile_id_range_sorted(report.bounds_deg, 5, 7);
        assert_eq!(
            parsed.entries.iter().map(|e| e.tile_id).collect::<Vec<_>>(),
            coords.iter().map(|(id, _)| *id).collect::<Vec<_>>()
        );

        // Metadata declares the terrain contract.
        for needle in [
            r#""format":"webp""#,
            r#""type":"baselayer""#,
            r#""encoding":"mapbox""#,
            r#""minzoom":"5""#,
            r#""maxzoom":"7""#,
        ] {
            assert!(parsed.metadata.contains(needle), "metadata: {needle}");
        }

        std::fs::remove_dir_all(&dir).ok();
    }
}
