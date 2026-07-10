import fs from 'fs';
import path from 'path';
import { execSync } from 'child_process';

const DATA_DIR = path.resolve('valhalla_data');
const PBF_URL = 'https://download.geofabrik.de/europe/andorra-latest.osm.pbf';
const PBF_FILE = path.join(DATA_DIR, 'andorra-latest.osm.pbf');
const TARGET_TAR = path.resolve('public/test_graph.tar');

async function downloadFile(url, destPath) {
  console.log(`[Graph Builder] Downloading ${url}...`);
  const response = await fetch(url);
  if (!response.ok) {
    throw new Error(`Failed to download: HTTP ${response.status}`);
  }
  const arrayBuffer = await response.arrayBuffer();
  fs.writeFileSync(destPath, Buffer.from(arrayBuffer));
  console.log('[Graph Builder] Download complete.');
}

function runCommand(cmd) {
  console.log(`[Graph Builder] Running: ${cmd}`);
  execSync(cmd, { stdio: 'inherit' });
}

async function main() {
  try {
    // 1. Create directory
    if (!fs.existsSync(DATA_DIR)) {
      fs.mkdirSync(DATA_DIR, { recursive: true });
    }

    // 2. Download PBF
    await downloadFile(PBF_URL, PBF_FILE);

    // 3. Execute Valhalla Docker commands
    console.log('[Graph Builder] Starting Valhalla Docker build process...');
    
    // Command 1: Build config
    console.log('[Graph Builder] Step 1/3: Building config valhalla.json...');
    const configCmd = `docker run --rm -v "${DATA_DIR}":/custom_files ghcr.io/gis-ops/docker-valhalla/valhalla:latest valhalla_build_config --mjolnir-tile-dir /custom_files/valhalla_tiles --mjolnir-tile-extract /custom_files/test_graph.tar --mjolnir-timezone /custom_files/valhalla_tiles/timezones.sqlite --mjolnir-admin /custom_files/valhalla_tiles/admins.sqlite > "${path.join(DATA_DIR, 'valhalla.json')}"`;
    runCommand(configCmd);

    // Command 2: Build routing tiles
    console.log('[Graph Builder] Step 2/3: Building tiles from OSM PBF...');
    const tilesCmd = `docker run --rm -v "${DATA_DIR}":/custom_files ghcr.io/gis-ops/docker-valhalla/valhalla:latest valhalla_build_tiles -c /custom_files/valhalla.json /custom_files/andorra-latest.osm.pbf`;
    runCommand(tilesCmd);

    // Command 3: Build extract tar file
    console.log('[Graph Builder] Step 3/3: Bundling tiles into test_graph.tar extract...');
    const extractCmd = `docker run --rm -v "${DATA_DIR}":/custom_files ghcr.io/gis-ops/docker-valhalla/valhalla:latest valhalla_build_extract -c /custom_files/valhalla.json -v`;
    runCommand(extractCmd);

    // 4. Provision to public folder
    const generatedTar = path.join(DATA_DIR, 'test_graph.tar');
    if (fs.existsSync(generatedTar)) {
      console.log(`[Graph Builder] Moving test_graph.tar to ${TARGET_TAR}...`);
      
      // Ensure target directory public/ exists
      const publicDir = path.dirname(TARGET_TAR);
      if (!fs.existsSync(publicDir)) {
        fs.mkdirSync(publicDir, { recursive: true });
      }

      fs.copyFileSync(generatedTar, TARGET_TAR);
      console.log('[Graph Builder] test_graph.tar successfully copied to public folder!');
    } else {
      throw new Error('test_graph.tar was not found in the generated build folder.');
    }

    // 5. Cleanup temporary folder
    console.log('[Graph Builder] Cleaning up temporary build directory...');
    fs.rmSync(DATA_DIR, { recursive: true, force: true });
    console.log('[Graph Builder] Temporary directory cleaned up.');
    console.log('[Graph Builder] Graph compilation completed successfully!');

  } catch (err) {
    console.error('[Graph Builder] Fatal build error:', err);
    process.exit(1);
  }
}

main();
