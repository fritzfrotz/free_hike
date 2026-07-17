//! `terrain-tile` — dev CLI for P6.C1/C2: cut one Terrain-RGB WebP tile from
//! a DEM GeoTIFF and write it to disk.
//!
//! Usage: terrain-tile <dem.tif> <out.webp> [col row | z/x/y]
//!   col row — raw DEM chunk window (P6.C1 path); defaults to 0 0
//!   z/x/y   — WebMercator pyramid tile, reprojected + bilinear-resampled

use std::path::PathBuf;
use std::process::ExitCode;

use terrain::mercator::{tile_bounds_deg, TileCoord};
use terrain::reader::WindowedDemReader;
use terrain::sample::DemSampler;
use terrain::{pyramid, rgb, webp};

const USAGE: &str = "usage: terrain-tile <dem.tif> <out.webp> [col row | z/x/y]";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("terrain-tile: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let dem: PathBuf = args.next().ok_or(USAGE)?.into();
    let out: PathBuf = args.next().ok_or(USAGE)?.into();

    let first = args.next();
    if let Some(zxy) = first.as_deref().filter(|a| a.contains('/')) {
        return run_pyramid(&dem, &out, zxy);
    }
    let col: u32 = first.map(|a| a.parse()).transpose()?.unwrap_or(0);
    let row: u32 = args.next().map(|a| a.parse()).transpose()?.unwrap_or(0);

    let mut reader = WindowedDemReader::open(&dem)?;
    let (w, h) = reader.dimensions();
    let (cw, ch) = reader.chunk_dimensions();
    let (cols, rows) = reader.window_grid();
    println!(
        "dem {}: {w}×{h} px, {cw}×{ch} chunks in a {cols}×{rows} grid, nodata {:?}",
        dem.display(),
        reader.nodata()
    );

    let window = reader.read_window(col, row)?;
    match window.elevation_range() {
        Some((lo, hi)) => println!(
            "window ({col},{row}): {}×{} px, elevation {lo:.1}m … {hi:.1}m",
            window.width, window.height
        ),
        None => println!(
            "window ({col},{row}): {}×{} px, all NoData",
            window.width, window.height
        ),
    }

    let rgb_buf = rgb::window_to_terrain_rgb(&window)?;
    let tile = webp::encode_rgb_lossless(&rgb_buf, rgb::TILE_SIZE as u32, rgb::TILE_SIZE as u32)?;
    std::fs::write(&out, &tile)?;
    println!(
        "wrote {} bytes of lossless 256×256 Terrain-RGB WebP → {}",
        tile.len(),
        out.display()
    );
    Ok(())
}

fn run_pyramid(
    dem: &std::path::Path,
    out: &std::path::Path,
    zxy: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut parts = zxy.split('/');
    let coord = TileCoord {
        z: parts.next().ok_or(USAGE)?.parse()?,
        x: parts.next().ok_or(USAGE)?.parse()?,
        y: parts.next().ok_or(USAGE)?.parse()?,
    };
    if parts.next().is_some() {
        return Err(USAGE.into());
    }

    let (lon_min, lat_min, lon_max, lat_max) = tile_bounds_deg(coord);
    println!("tile {coord}: lon {lon_min:.4}…{lon_max:.4}, lat {lat_min:.4}…{lat_max:.4}");

    let mut sampler = DemSampler::open(dem)?;
    let gt = sampler.transform();
    println!(
        "dem {}: origin ({:.6}, {:.6}), scale ({:.6}, {:.6})",
        dem.display(),
        gt.origin_x,
        gt.origin_y,
        gt.scale_x,
        gt.scale_y
    );

    let tile = pyramid::render_tile(&mut sampler, coord)?;
    std::fs::write(out, &tile.webp)?;
    println!(
        "wrote {} bytes of lossless 256×256 Terrain-RGB WebP → {}",
        tile.webp.len(),
        out.display()
    );
    Ok(())
}
