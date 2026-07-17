//! `terrain-tile` — dev CLI for P6.C1: cut one Terrain-RGB WebP tile from a
//! DEM GeoTIFF window and write it to disk.
//!
//! Usage: terrain-tile <dem.tif> <out.webp> [col] [row]   (window defaults 0 0)

use std::path::PathBuf;
use std::process::ExitCode;

use terrain::reader::WindowedDemReader;
use terrain::{rgb, webp};

const USAGE: &str = "usage: terrain-tile <dem.tif> <out.webp> [col] [row]";

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
    let col: u32 = args.next().map(|a| a.parse()).transpose()?.unwrap_or(0);
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
