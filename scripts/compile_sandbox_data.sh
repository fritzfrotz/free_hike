#!/usr/bin/env bash

# compile_sandbox_data.sh
# 
# Automates the compilation of Innsbruck/Tyrol vector and terrain PMTiles.
# Assumes 'planetiler.jar' is located in the project root, and 'innsbruck_dem.tif'
# is manually placed in 'offline_sandbox/raw_data/' before executing.

set -euo pipefail

# Ensure Cargo binaries are in PATH (for massif)
export PATH="${HOME}/.cargo/bin:${PATH}"

# Ensure we are in the project root directory
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${PROJECT_ROOT}"

echo "=== FreeHike Map Data Compilation Pipeline ==="
echo "Project root resolved to: ${PROJECT_ROOT}"

# 1. Directory Setup
echo "Creating sandbox directories..."
mkdir -p offline_sandbox/raw_data
mkdir -p offline_sandbox/output
mkdir -p public/local

# 2. Download + clip Vector Data
#
# Geofabrik does NOT publish a per-state extract for Austria (there is no
# europe/austria/tirol-latest.osm.pbf — that path 302-redirects to the
# Geofabrik homepage, which `curl -L` will happily save as a "successful"
# download unless its content is explicitly validated). The only Austria
# extract Geofabrik offers is the whole country, so we download that once,
# cache it, and clip it down to the Innsbruck/Tyrol bounding box with
# osmium-tool (`brew install osmium-tool`).
AUSTRIA_URL="https://download.geofabrik.de/europe/austria-latest.osm.pbf"
AUSTRIA_CACHE="offline_sandbox/raw_data/_cache/austria-latest.osm.pbf"
OSM_DEST="offline_sandbox/raw_data/innsbruck.osm.pbf"
# Innsbruck / Nordkette / Patscherkofel + margin: west,south,east,north
INNSBRUCK_BBOX="11.15,47.05,11.65,47.45"

if ! command -v osmium >/dev/null 2>&1; then
  echo "Error: 'osmium' CLI not found on PATH. Install it with 'brew install osmium-tool'."
  exit 1
fi

mkdir -p offline_sandbox/raw_data/_cache

if [ ! -f "${AUSTRIA_CACHE}" ]; then
  echo "Downloading full Austria OpenStreetMap extract from Geofabrik (~800MB)..."
  if command -v curl >/dev/null 2>&1; then
    curl -L --fail --retry 3 -o "${AUSTRIA_CACHE}" "${AUSTRIA_URL}"
  elif command -v wget >/dev/null 2>&1; then
    wget -O "${AUSTRIA_CACHE}" "${AUSTRIA_URL}"
  else
    echo "Error: Neither curl nor wget is installed. Cannot download OSM extract."
    exit 1
  fi

  # Validate the download is actually a PBF file, not an HTML error/redirect
  # page saved with a 200 status (the exact failure mode that previously
  # corrupted this pipeline: a redirect got silently saved as innsbruck.osm.pbf).
  if file "${AUSTRIA_CACHE}" | grep -qi "HTML"; then
    echo "ERROR: Geofabrik download failed or returned an HTML page instead of PBF data!"
    rm -f "${AUSTRIA_CACHE}"
    exit 1
  fi
  echo "Austria extract download completed and verified."
else
  echo "Found cached Austria extract at ${AUSTRIA_CACHE}, skipping download."
fi

echo "Clipping Austria extract to the Innsbruck/Tyrol bounding box (${INNSBRUCK_BBOX})..."
osmium extract -b "${INNSBRUCK_BBOX}" -o "${OSM_DEST}" "${AUSTRIA_CACHE}" --overwrite
echo "Clipped extract written to ${OSM_DEST}."

# 3. Compile Vector PMTiles (Planetiler)
PLANETILER_JAR="planetiler.jar"
BASEMAP_PMTILES="offline_sandbox/output/alps_basemap.pmtiles"

if [ -f "${PLANETILER_JAR}" ]; then
  echo "Compiling vector PMTiles with Planetiler..."
  # NOTE: the flag is --osm_path, NOT --openstreetmap (which Planetiler
  # silently ignores as an unrecognized argument, then falls back to its
  # `area=monaco` default — this is what previously caused the Alps basemap
  # to be built from Monaco's OSM data regardless of what OSM_DEST pointed
  # to). Likewise the overwrite flag is --force, not --overwrite.
  java -jar "${PLANETILER_JAR}" \
    --osm_path="${OSM_DEST}" \
    --output="${BASEMAP_PMTILES}" \
    --profile=protomaps \
    --maxzoom=14 \
    --nodata \
    --download \
    --force
  echo "Vector compilation completed: ${BASEMAP_PMTILES}"
else
  echo "WARNING: ${PLANETILER_JAR} not found in project root. Skipping vector compilation."
  echo "Please place ${PLANETILER_JAR} in the project root to compile vector data."
fi

# 4. Compile Terrain PMTiles (Massif)
#
# Massif is vendored as Rust source under offline_sandbox/massif (no
# .gitmodules / real submodule wiring, so `cargo build` must run from
# source rather than assuming a globally-installed `massif` on PATH).
DEM_TIF="offline_sandbox/raw_data/innsbruck_dem.tif"
TERRAIN_PMTILES="offline_sandbox/output/alps_terrain.pmtiles"
MASSIF_DIR="offline_sandbox/massif"
MASSIF_BIN="${MASSIF_DIR}/target/release/massif"

if [ -f "${DEM_TIF}" ]; then
  if [ ! -x "${MASSIF_BIN}" ]; then
    if ! command -v cargo >/dev/null 2>&1; then
      echo "WARNING: 'cargo' not found on PATH. Cannot build massif from source. Skipping terrain compilation."
      echo "Install Rust (https://rustup.rs) to enable terrain generation."
      MASSIF_BIN=""
    else
      echo "Building massif from vendored source (${MASSIF_DIR})..."
      (cd "${MASSIF_DIR}" && cargo build --release)
    fi
  fi

  if [ -n "${MASSIF_BIN}" ] && [ -x "${MASSIF_BIN}" ]; then
    echo "Compiling Terrain-RGB PMTiles with Massif..."
    "${MASSIF_BIN}" \
      --encoding mapbox \
      --format webp \
      --compress 6 \
      -r 3 \
      --min-z 5 \
      --max-z 12 \
      -b -10000 \
      -i 0.1 \
      "${DEM_TIF}" \
      "${TERRAIN_PMTILES}"
    echo "Terrain compilation completed: ${TERRAIN_PMTILES}"
  fi
else
  echo "WARNING: ${DEM_TIF} not found. Skipping terrain compilation."
  echo "Please place innsbruck_dem.tif in offline_sandbox/raw_data/ to compile terrain data."
fi

# 5. Move/Copy to Public
echo "Copying compiled assets to public directory..."
if [ -f "${BASEMAP_PMTILES}" ]; then
  cp "${BASEMAP_PMTILES}" public/local/
  echo "Copied alps_basemap.pmtiles to public/local/"
fi

if [ -f "${TERRAIN_PMTILES}" ]; then
  cp "${TERRAIN_PMTILES}" public/local/
  echo "Copied alps_terrain.pmtiles to public/local/"
fi

echo "=== Pipeline Finished ==="
