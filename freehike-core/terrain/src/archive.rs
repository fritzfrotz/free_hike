// SPDX-License-Identifier: Apache-2.0
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
//!
//! P6.C4 wraps the render loop in the Surface-v1 budget-yield contract:
//! [`run_archive_slice`] renders until the deadline, then persists a
//! [`TerrainCheckpoint`] (engine-style `key=value` text, tmp+fsync+rename)
//! and yields. Resume rebuilds the directory by walking the RIFF-delimited
//! data temp file up to the checkpointed high-water mark — payload bytes are
//! self-describing, so the checkpoint stays fixed-size no matter how large
//! the pyramid grows. This cursor is terrain-local (its own version counter,
//! starting at 1); the engine's redb-backed v5 checkpoint is untouched.

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

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

/// Slice result, mirroring the engine's contract: `Yielded` means the
/// budget expired with a durable checkpoint on disk — call again with the
/// same arguments to resume; `Finished` means the archive is in place and
/// all temporary state (data file, checkpoint) is purged.
#[derive(Debug)]
pub enum SliceOutcome {
    Yielded(TerrainCheckpoint),
    Finished(ArchiveReport),
}

/// Terrain's own cursor version — bump on ANY change to the checkpoint
/// fields or the data-file recovery contract (house rule). Independent of
/// the engine's redb checkpoint (v5).
pub const TERRAIN_CHECKPOINT_VERSION: u32 = 1;

/// Durable position inside a partially rendered pyramid. The identity
/// fields (zoom range, bounds as exact f64 bit patterns) guard against
/// resuming against a different DEM or parameter set — the enumeration must
/// be the same one the checkpoint was cut from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TerrainCheckpoint {
    pub min_zoom: u8,
    pub max_zoom: u8,
    pub bounds_deg: (f64, f64, f64, f64),
    /// Hilbert tile ID of the last durably written tile.
    pub last_tile_id: u64,
    pub tiles_written: u64,
    /// Data-section high-water mark; everything beyond it is torn tail.
    pub bytes_written: u64,
}

fn checkpoint_path(out_path: &Path) -> PathBuf {
    out_path.with_extension("terrain.checkpoint")
}

/// Atomic, durable checkpoint write: temp file → fsync → rename (the
/// engine's save_checkpoint pattern). Bounds are serialized as f64 bit
/// patterns so the resume comparison is exact, not print-precision.
fn save_checkpoint(path: &Path, cp: &TerrainCheckpoint) -> Result<(), TerrainError> {
    let (w, s, e, n) = cp.bounds_deg;
    let body = format!(
        "version={TERRAIN_CHECKPOINT_VERSION}\nmin_zoom={}\nmax_zoom={}\nbounds_bits={:016x},{:016x},{:016x},{:016x}\nlast_tile_id={}\ntiles_written={}\nbytes_written={}\n",
        cp.min_zoom,
        cp.max_zoom,
        w.to_bits(),
        s.to_bits(),
        e.to_bits(),
        n.to_bits(),
        cp.last_tile_id,
        cp.tiles_written,
        cp.bytes_written,
    );
    let tmp = path.with_extension("checkpoint.tmp");
    let mut f = File::create(&tmp)?;
    f.write_all(body.as_bytes())?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Loads a checkpoint if present. `Ok(None)` = fresh start. Malformed
/// content is a hard error — a torn or foreign file must never silently
/// restart (and thus duplicate or interleave) work.
fn load_checkpoint(path: &Path) -> Result<Option<TerrainCheckpoint>, TerrainError> {
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let corrupt = |what: &str| TerrainError::Corrupt(format!("terrain checkpoint: {what}"));

    let mut version = None;
    let mut min_zoom = None;
    let mut max_zoom = None;
    let mut bounds = None;
    let mut last_tile_id = None;
    let mut tiles_written = None;
    let mut bytes_written = None;
    for line in raw.lines() {
        let Some((k, v)) = line.split_once('=') else {
            return Err(corrupt(&format!("malformed line '{line}'")));
        };
        match k {
            "version" => version = v.parse::<u32>().ok(),
            "min_zoom" => min_zoom = v.parse::<u8>().ok(),
            "max_zoom" => max_zoom = v.parse::<u8>().ok(),
            "bounds_bits" => {
                let bits: Vec<u64> = v
                    .split(',')
                    .filter_map(|p| u64::from_str_radix(p, 16).ok())
                    .collect();
                if bits.len() == 4 {
                    bounds = Some((
                        f64::from_bits(bits[0]),
                        f64::from_bits(bits[1]),
                        f64::from_bits(bits[2]),
                        f64::from_bits(bits[3]),
                    ));
                }
            }
            "last_tile_id" => last_tile_id = v.parse::<u64>().ok(),
            "tiles_written" => tiles_written = v.parse::<u64>().ok(),
            "bytes_written" => bytes_written = v.parse::<u64>().ok(),
            other => return Err(corrupt(&format!("unknown key '{other}'"))),
        }
    }
    match (
        version,
        min_zoom,
        max_zoom,
        bounds,
        last_tile_id,
        tiles_written,
        bytes_written,
    ) {
        (Some(v), ..) if v != TERRAIN_CHECKPOINT_VERSION => Err(corrupt(&format!(
            "version {v}, expected {TERRAIN_CHECKPOINT_VERSION}"
        ))),
        (
            Some(_),
            Some(min_zoom),
            Some(max_zoom),
            Some(bounds_deg),
            Some(last),
            Some(tiles),
            Some(bytes),
        ) => Ok(Some(TerrainCheckpoint {
            min_zoom,
            max_zoom,
            bounds_deg,
            last_tile_id: last,
            tiles_written: tiles,
            bytes_written: bytes,
        })),
        _ => Err(corrupt("missing field")),
    }
}

/// Rebuilds directory entries from the data temp file up to `upto` bytes.
/// WebP payloads are RIFF-delimited (bytes 4..8 = little-endian chunk size,
/// total payload = size + 8), so offsets and lengths recover from the bytes
/// themselves; tile IDs pair positionally with the deterministic sorted
/// enumeration.
fn rebuild_entries(
    data: &mut File,
    upto: u64,
    coords: &[(u64, TileCoord)],
) -> Result<Vec<DirEntry>, TerrainError> {
    let corrupt = |what: String| TerrainError::Corrupt(format!("terrain data tmp: {what}"));
    let mut entries = Vec::new();
    let mut offset = 0u64;
    while offset < upto {
        data.seek(SeekFrom::Start(offset))?;
        let mut header = [0u8; 8];
        data.read_exact(&mut header)?;
        if &header[0..4] != b"RIFF" {
            return Err(corrupt(format!("no RIFF magic at offset {offset}")));
        }
        let length = u64::from(u32::from_le_bytes(header[4..8].try_into().unwrap())) + 8;
        let (id, _) = *coords
            .get(entries.len())
            .ok_or_else(|| corrupt("more payloads than enumerated tiles".into()))?;
        entries.push(DirEntry {
            tile_id: id,
            offset,
            length: length as u32,
            run_length: 1,
        });
        offset += length;
    }
    if offset != upto {
        return Err(corrupt(format!(
            "payload boundary {offset} overshoots high-water mark {upto}"
        )));
    }
    Ok(entries)
}

/// Builds `terrain.pmtiles` at `out_path` covering the DEM's full extent for
/// zooms `min_zoom..=max_zoom`, in one uninterrupted run (no deadline). If a
/// checkpoint from a killed sliced run exists, it resumes rather than
/// re-rendering. Atomic: written beside the target and renamed into place.
pub fn build_terrain_archive<R: Read + Seek>(
    sampler: &mut DemSampler<R>,
    out_path: &Path,
    min_zoom: u8,
    max_zoom: u8,
) -> Result<ArchiveReport, TerrainError> {
    match run_slice_inner(sampler, out_path, min_zoom, max_zoom, None)? {
        SliceOutcome::Finished(report) => Ok(report),
        SliceOutcome::Yielded(_) => unreachable!("no deadline, cannot yield"),
    }
}

/// One budget-bounded slice of archive assembly (Surface-v1 contract). At
/// least one tile makes progress per slice regardless of budget; the budget
/// is checked after each tile's render + disk write. Assembly itself runs
/// only once every tile is durable — a spent budget yields BETWEEN render
/// exhaustion and assembly, mirroring the P5 encode/assemble split.
pub fn run_archive_slice<R: Read + Seek>(
    sampler: &mut DemSampler<R>,
    out_path: &Path,
    min_zoom: u8,
    max_zoom: u8,
    budget: Duration,
) -> Result<SliceOutcome, TerrainError> {
    run_slice_inner(
        sampler,
        out_path,
        min_zoom,
        max_zoom,
        Some(Instant::now() + budget),
    )
}

fn run_slice_inner<R: Read + Seek>(
    sampler: &mut DemSampler<R>,
    out_path: &Path,
    min_zoom: u8,
    max_zoom: u8,
    deadline: Option<Instant>,
) -> Result<SliceOutcome, TerrainError> {
    assert!(min_zoom <= max_zoom, "inverted zoom range");
    let bounds = sampler
        .reader()
        .geo_bounds()
        .ok_or(DemError::MissingGeoTransform)?;

    // Enumerate the whole pyramid and sort by Hilbert tile ID — ascending
    // IDs order z5 before z12 for free (the ID space is zoom-prefixed).
    let coords = tile_id_range_sorted(bounds, min_zoom, max_zoom);

    let data_tmp = out_path.with_extension("data.tmp");
    let ckpt_path = checkpoint_path(out_path);
    let corrupt = |what: &str| TerrainError::Corrupt(format!("terrain resume: {what}"));

    // Resume from a durable checkpoint, or start fresh. Resume validates
    // the full identity (version, zoom range, exact bounds bits, cursor
    // position inside today's enumeration) before touching the data file.
    let (mut dir, mut data_len, data) = match load_checkpoint(&ckpt_path)? {
        Some(cp) => {
            if (cp.min_zoom, cp.max_zoom) != (min_zoom, max_zoom) {
                return Err(corrupt("checkpoint zoom range differs from request"));
            }
            let (w, s, e, n) = cp.bounds_deg;
            let (bw, bs, be, bn) = bounds;
            if [w, s, e, n].map(f64::to_bits) != [bw, bs, be, bn].map(f64::to_bits) {
                return Err(corrupt("checkpoint bounds differ from DEM"));
            }
            if cp.tiles_written > coords.len() as u64 {
                return Err(corrupt("checkpoint cursor beyond enumeration"));
            }
            if cp.tiles_written > 0 && coords[cp.tiles_written as usize - 1].0 != cp.last_tile_id {
                return Err(corrupt("checkpoint cursor disagrees with enumeration"));
            }
            let mut f = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&data_tmp)
                .map_err(|_| corrupt("checkpoint present but data file missing"))?;
            if f.metadata()?.len() < cp.bytes_written {
                return Err(corrupt("data file shorter than checkpoint high-water mark"));
            }
            // Torn tail from a crash mid-append: everything beyond the
            // durable mark is discarded before appending resumes.
            f.set_len(cp.bytes_written)?;
            let entries = rebuild_entries(&mut f, cp.bytes_written, &coords)?;
            if entries.len() as u64 != cp.tiles_written {
                return Err(corrupt("recovered entry count disagrees with checkpoint"));
            }
            f.seek(SeekFrom::Start(cp.bytes_written))?;
            (entries, cp.bytes_written, f)
        }
        None => (Vec::new(), 0u64, File::create(&data_tmp)?),
    };

    // Render in ID order, streaming payloads to the data temp file so peak
    // memory is one tile regardless of pyramid size.
    let mut writer = BufWriter::new(data);
    for &(id, coord) in &coords[dir.len()..] {
        let tile = render_tile(sampler, coord)?;
        writer.write_all(&tile.webp)?;
        dir.push(DirEntry {
            tile_id: id,
            offset: data_len,
            length: tile.webp.len() as u32,
            run_length: 1,
        });
        data_len += tile.webp.len() as u64;

        if deadline.is_some_and(|d| Instant::now() >= d) {
            // Payload bytes durable BEFORE the checkpoint that references
            // them — the cursor never runs ahead of data (P5 house rule).
            writer.flush()?;
            writer.get_ref().sync_all()?;
            let cp = TerrainCheckpoint {
                min_zoom,
                max_zoom,
                bounds_deg: bounds,
                last_tile_id: id,
                tiles_written: dir.len() as u64,
                bytes_written: data_len,
            };
            save_checkpoint(&ckpt_path, &cp)?;
            return Ok(SliceOutcome::Yielded(cp));
        }
    }
    writer.flush()?;
    writer.get_ref().sync_all()?;
    drop(writer);

    let report = assemble(
        out_path, &data_tmp, &dir, data_len, bounds, min_zoom, max_zoom,
    )?;
    // Purge order is load-bearing for kill-safety: checkpoint FIRST. A crash
    // after the archive rename but before either purge leaves ckpt+data — a
    // re-run resumes and reassembles idempotently. A crash between the two
    // purges leaves only the orphan data file, which a fresh start truncates.
    // The reverse order could strand a checkpoint without its data file,
    // which resume (correctly) refuses as corrupt.
    let _ = std::fs::remove_file(&ckpt_path);
    let _ = std::fs::remove_file(&data_tmp);
    Ok(SliceOutcome::Finished(report))
}

/// Final archive assembly from a complete directory + data temp file.
/// Idempotent (atomic tmp + rename); the caller owns temp-state purging —
/// in the crash-safe order documented at the call site.
fn assemble(
    out_path: &Path,
    data_tmp: &Path,
    dir: &[DirEntry],
    data_len: u64,
    bounds: (f64, f64, f64, f64),
    min_zoom: u8,
    max_zoom: u8,
) -> Result<ArchiveReport, TerrainError> {
    let root_dir = gzip(&serialize_directory(dir));
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
    let mut data = File::open(data_tmp)?;
    std::io::copy(&mut data, &mut out)?;
    out.flush()?;
    out.get_ref().sync_all()?;
    drop(out);
    std::fs::rename(&archive_tmp, out_path)?;

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

    /// The task-required P6.C4 proof: a 0ms budget forces a yield after
    /// every tile (minimum-progress guarantee), each slice resumes from the
    /// durable checkpoint with a FRESH sampler (simulating process death),
    /// and the final archive is byte-identical to an uninterrupted run —
    /// including surviving a torn tail scribbled past the high-water mark.
    #[test]
    fn zero_budget_slices_resume_to_byte_identical_archive() {
        let dir = std::env::temp_dir().join(format!("terrain-slices-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mono = dir.join("mono.pmtiles");
        let sliced = dir.join("sliced.pmtiles");

        build_terrain_archive(&mut sampler(), &mono, 5, 7).unwrap();

        // z5–7 over the synthetic DEM = 6 tiles → exactly 6 yields, then a
        // finishing slice that only assembles.
        let mut yields = 0u32;
        let report = loop {
            // Fresh sampler per slice: nothing carries over but disk state.
            match run_archive_slice(&mut sampler(), &sliced, 5, 7, Duration::ZERO).unwrap() {
                SliceOutcome::Yielded(cp) => {
                    yields += 1;
                    assert_eq!(cp.tiles_written, u64::from(yields), "one tile per slice");
                    assert!(cp.bytes_written > 0);
                    if yields == 2 {
                        // Kill-torture: a crash mid-append leaves a torn
                        // tail past the checkpointed mark; resume must
                        // discard it, not serve it.
                        let mut f = OpenOptions::new()
                            .append(true)
                            .open(sliced.with_extension("data.tmp"))
                            .unwrap();
                        f.write_all(b"TORNTAILGARBAGE").unwrap();
                    }
                }
                SliceOutcome::Finished(r) => break r,
            }
            assert!(yields < 100, "slices stopped making progress");
        };
        assert_eq!(yields, 6);
        assert_eq!(report.tile_count, 6);

        // Byte-identical to the uninterrupted run; all temp state purged.
        assert_eq!(
            std::fs::read(&mono).unwrap(),
            std::fs::read(&sliced).unwrap(),
            "sliced archive must equal the monolithic one byte-for-byte"
        );
        assert!(!checkpoint_path(&sliced).exists());
        assert!(!sliced.with_extension("data.tmp").exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resume_rejects_foreign_or_mismatched_checkpoints() {
        let dir = std::env::temp_dir().join(format!("terrain-ckpt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("t.pmtiles");

        // Cut one real checkpoint.
        let cp = match run_archive_slice(&mut sampler(), &out, 5, 7, Duration::ZERO).unwrap() {
            SliceOutcome::Yielded(cp) => cp,
            SliceOutcome::Finished(_) => panic!("0ms budget must yield"),
        };
        assert_eq!(cp.tiles_written, 1);

        // Different zoom range: the enumeration would not match the cursor.
        let err = run_archive_slice(&mut sampler(), &out, 5, 8, Duration::ZERO).unwrap_err();
        assert!(matches!(err, TerrainError::Corrupt(_)), "got {err}");

        // Checkpoint present but data file gone.
        std::fs::remove_file(out.with_extension("data.tmp")).unwrap();
        let err = run_archive_slice(&mut sampler(), &out, 5, 7, Duration::ZERO).unwrap_err();
        assert!(matches!(err, TerrainError::Corrupt(_)), "got {err}");

        // Torn checkpoint file (malformed line) is a hard error, not a
        // silent restart.
        std::fs::write(checkpoint_path(&out), "not a checkpoint").unwrap();
        let err = run_archive_slice(&mut sampler(), &out, 5, 7, Duration::ZERO).unwrap_err();
        assert!(matches!(err, TerrainError::Corrupt(_)), "got {err}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn finish_crash_window_reassembles_idempotently() {
        // The worst finish-path crash: the archive was renamed into place
        // but the process died before EITHER purge (checkpoint and data file
        // both still on disk). A re-entry must resume, reassemble the
        // identical archive over the existing one, and complete cleanup —
        // never error, never double-render.
        let dir = std::env::temp_dir().join(format!("terrain-window-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("t.pmtiles");

        // Drive to the final yield: all 6 tiles durable, assembly pending.
        let mut yields = 0;
        while yields < 6 {
            match run_archive_slice(&mut sampler(), &out, 5, 7, Duration::ZERO).unwrap() {
                SliceOutcome::Yielded(_) => yields += 1,
                SliceOutcome::Finished(_) => panic!("assembly before all yields"),
            }
        }
        // Simulate the crash window: assembly ran (archive exists) but the
        // purges did not. Plant the archive by snapshotting temp state,
        // finishing once, then restoring ckpt + data beside the result.
        let ckpt_bytes = std::fs::read(checkpoint_path(&out)).unwrap();
        let data_bytes = std::fs::read(out.with_extension("data.tmp")).unwrap();
        match run_archive_slice(&mut sampler(), &out, 5, 7, Duration::ZERO).unwrap() {
            SliceOutcome::Finished(r) => assert_eq!(r.tile_count, 6),
            SliceOutcome::Yielded(_) => panic!("nothing left to render"),
        }
        let finished = std::fs::read(&out).unwrap();
        std::fs::write(checkpoint_path(&out), &ckpt_bytes).unwrap();
        std::fs::write(out.with_extension("data.tmp"), &data_bytes).unwrap();

        // Re-entry over the planted crash state.
        match run_archive_slice(&mut sampler(), &out, 5, 7, Duration::ZERO).unwrap() {
            SliceOutcome::Finished(r) => assert_eq!(r.tile_count, 6),
            SliceOutcome::Yielded(_) => panic!("re-entry must go straight to assembly"),
        }
        assert_eq!(std::fs::read(&out).unwrap(), finished, "idempotent bytes");
        assert!(!checkpoint_path(&out).exists());
        assert!(!out.with_extension("data.tmp").exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn monolithic_build_resumes_a_killed_sliced_run() {
        let dir = std::env::temp_dir().join(format!("terrain-resume-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mono = dir.join("mono.pmtiles");
        let killed = dir.join("killed.pmtiles");

        build_terrain_archive(&mut sampler(), &mono, 5, 7).unwrap();

        // Simulate a run killed after three tiles…
        for _ in 0..3 {
            match run_archive_slice(&mut sampler(), &killed, 5, 7, Duration::ZERO).unwrap() {
                SliceOutcome::Yielded(_) => {}
                SliceOutcome::Finished(_) => panic!("must still be mid-pyramid"),
            }
        }
        // …then a plain build picks the checkpoint up and completes.
        let report = build_terrain_archive(&mut sampler(), &killed, 5, 7).unwrap();
        assert_eq!(report.tile_count, 6);
        assert_eq!(
            std::fs::read(&mono).unwrap(),
            std::fs::read(&killed).unwrap()
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
