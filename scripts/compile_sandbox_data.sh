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

# 2. Download Vector Data
OSM_URL="https://download.geofabrik.de/europe/austria/tirol-latest.osm.pbf"
OSM_DEST="offline_sandbox/raw_data/innsbruck.osm.pbf"

if [ ! -f "${OSM_DEST}" ]; then
  echo "Downloading Innsbruck/Tyrol OpenStreetMap extract from Geofabrik..."
  if command -v curl >/dev/null 2>&1; then
    curl -L -o "${OSM_DEST}" "${OSM_URL}"
  elif command -v wget >/dev/null 2>&1; then
    wget -O "${OSM_DEST}" "${OSM_URL}"
  else
    echo "Error: Neither curl nor wget is installed. Cannot download OSM extract."
    exit 1
  fi
  echo "OSM download completed."
else
  echo "Found existing OSM data at ${OSM_DEST}, skipping download."
fi

# 3. Compile Vector PMTiles (Planetiler)
PLANETILER_JAR="planetiler.jar"
BASEMAP_PMTILES="offline_sandbox/output/alps_basemap.pmtiles"

if [ -f "${PLANETILER_JAR}" ]; then
  echo "Compiling vector PMTiles with Planetiler..."
  java -jar "${PLANETILER_JAR}" \
    --openstreetmap="${OSM_DEST}" \
    --output="${BASEMAP_PMTILES}" \
    --profile=protomaps \
    --maxzoom=14 \
    --nodata \
    --download \
    --overwrite
  echo "Vector compilation completed: ${BASEMAP_PMTILES}"
else
  echo "WARNING: ${PLANETILER_JAR} not found in project root. Skipping vector compilation."
  echo "Please place ${PLANETILER_JAR} in the project root to compile vector data."
fi

# 4. Compile Terrain PMTiles (Massif)
DEM_TIF="offline_sandbox/raw_data/innsbruck_dem.tif"
TERRAIN_PMTILES="offline_sandbox/output/alps_terrain.pmtiles"

if command -v massif >/dev/null 2>&1; then
  if [ -f "${DEM_TIF}" ]; then
    echo "Compiling Terrain-RGB PMTiles with Massif..."
    massif \
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
  else
    echo "WARNING: ${DEM_TIF} not found. Skipping terrain compilation."
    echo "Please place innsbruck_dem.tif in offline_sandbox/raw_data/ to compile terrain data."
  fi
else
  echo "WARNING: 'massif' CLI not found on PATH. Skipping terrain compilation."
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
