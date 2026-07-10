import { createRouter, decodePolyline, type ValhallaRouter, type CostingModel } from '@jansoft/mbujkanji-valhalla-wasm';
import type {
  WorkerRequestMessage,
  WorkerResponseMessage,
  RouteCalculateRequestPayload,
  RouteTileFetchRequestPayload,
  RouteTileWriteRequestPayload,
  RouteCalculateSuccessPayload,
} from '../shared/types';

// Store opened sync access handles to avoid repeatedly opening/closing
const activeHandles = new Map<string, FileSystemSyncAccessHandle>();

let router: ValhallaRouter | null = null;

// Provision test_graph.tar from server / Vite public folder if not already cached in OPFS
async function provisionGraph(): Promise<void> {
  const filename = 'test_graph.tar';
  try {
    const root = await navigator.storage.getDirectory();
    let fileExists = false;
    try {
      await root.getFileHandle(filename);
      fileExists = true;
    } catch {
      // file doesn't exist
    }

    if (fileExists) {
      console.log('[Routing Worker] test_graph.tar already cached in OPFS.');
      return;
    }

    console.log('[Routing Worker] test_graph.tar not found in OPFS. Fetching from server...');
    const response = await fetch('/test_graph.tar');
    if (!response.ok) {
      throw new Error(`Failed to fetch test_graph.tar from server: ${response.statusText}`);
    }
    const buffer = await response.arrayBuffer();
    await writeRoutingTar(filename, buffer);
    console.log('[Routing Worker] test_graph.tar cached in OPFS successfully.');
  } catch (err) {
    console.error('[Routing Worker] Error provisioning test_graph.tar:', err);
  }
}

// Initialize the Valhalla WASM module
async function initValhalla(): Promise<void> {
  if (router) return;

  console.log('[Routing Worker] Initializing Valhalla WASM module...');
  router = createRouter();
  await router.init({
    wasmPath: '/valhalla.wasm',
    jsGluePath: '/valhalla.js',
  });
  console.log('[Routing Worker] Valhalla WASM module initialized.');

  // Check and fetch test_graph.tar
  await provisionGraph();

  // Load tiles from cached test_graph.tar in OPFS
  try {
    console.log('[Routing Worker] Reading test_graph.tar synchronously from OPFS to load into Valhalla...');
    const handle = await getAccessHandle('test_graph.tar');
    const size = handle.getSize();
    if (size > 0) {
      const buffer = new Uint8Array(size);
      const bytesRead = handle.read(buffer, { at: 0 });
      await router.loadTiles(buffer.buffer.slice(0, bytesRead), { regionId: 'test_region' });
      console.log('[Routing Worker] Valhalla tiles loaded successfully.');
    } else {
      console.warn('[Routing Worker] test_graph.tar is empty (0 bytes). Skipping loadTiles.');
    }
  } catch (err) {
    console.error('[Routing Worker] Failed to load routing tiles from OPFS:', err);
  }
}

// Retrieve or create a sync access handle for the given file in OPFS
async function getAccessHandle(filename: string): Promise<FileSystemSyncAccessHandle> {
  let handle = activeHandles.get(filename);
  if (!handle) {
    const root = await navigator.storage.getDirectory();
    const fileHandle = await root.getFileHandle(filename, { create: true });
    handle = await fileHandle.createSyncAccessHandle();
    activeHandles.set(filename, handle);
  }
  return handle;
}

// Write the whole TAR buffer into OPFS
async function writeRoutingTar(filename: string, buffer: ArrayBuffer): Promise<number> {
  const handle = await getAccessHandle(filename);
  // Reset file size to zero before writing new contents
  handle.truncate(0);
  const view = new Uint8Array(buffer);
  const bytesWritten = handle.write(view, { at: 0 });
  handle.flush();
  console.log(`[Routing Worker] Synchronously cached ${filename} to OPFS. Size: ${bytesWritten} bytes.`);
  return bytesWritten;
}

// Read specific byte range from a cached TAR in OPFS
export async function readRoutingTarBytes(filename: string, offset: number, length: number): Promise<ArrayBuffer> {
  const handle = await getAccessHandle(filename);
  const fileSize = handle.getSize();
  const readLength = Math.min(length, fileSize - offset);

  if (readLength <= 0) {
    return new ArrayBuffer(0);
  }

  const buffer = new Uint8Array(readLength);
  const bytesRead = handle.read(buffer, { at: offset });

  if (bytesRead < readLength) {
    return buffer.buffer.slice(0, bytesRead);
  }
  return buffer.buffer;
}

// Listen for messages from the main thread
self.addEventListener('message', async (event: MessageEvent<WorkerRequestMessage>) => {
  const { id, type, payload } = event.data;

  try {
    // Lazy-initialize the WASM module on any routing request
    await initValhalla();

    switch (type) {
      case 'ROUTE_CALCULATE_REQUEST': {
        const { startX, startY, endX, endY, costingModel } = payload as RouteCalculateRequestPayload;

        console.log(`[Routing Worker] Running Valhalla routing query from [${startX}, ${startY}] to [${endX}, ${endY}] via ${costingModel || 'pedestrian'}`);

        let coords: [number, number][] = [];
        let distanceMeters = 0;

        try {
          if (!router) {
            throw new Error('Valhalla router is not initialized.');
          }

          // Build routing request object
          const request = {
            locations: [
              { lat: startY, lon: startX },
              { lat: endY, lon: endX }
            ],
            costing: (costingModel || 'pedestrian') as CostingModel,
            units: 'kilometers' as const,
            shape_format: 'polyline6' as const,
          };

          const routeResult = await router.route(request);
          if (routeResult && routeResult.trip && routeResult.trip.legs && routeResult.trip.legs.length > 0) {
            const leg = routeResult.trip.legs[0];
            // Decode encoded polyline6 shape string to [lon, lat][] array
            coords = decodePolyline(leg.shape, 'polyline6');
            distanceMeters = routeResult.trip.summary.length * 1000; // Valhalla returns length in km, convert to meters
          } else {
            throw new Error('Invalid or empty route response leg from Valhalla.');
          }
        } catch (err) {
          console.error('[Routing Worker] Valhalla routing call failed:', err);
          // Return empty coordinates and 0 distance on failure
          coords = [];
          distanceMeters = 0;
        }

        // Flatten coordinates to a Float64Array for transferable zero-copy messaging
        const flatCoords = new Float64Array(coords.length * 2);
        for (let i = 0; i < coords.length; i++) {
          flatCoords[i * 2] = coords[i][0];
          flatCoords[i * 2 + 1] = coords[i][1];
        }
        const coordsBuffer = flatCoords.buffer;

        const successPayload: RouteCalculateSuccessPayload = {
          geojson: {
            type: 'Feature',
            geometry: {
              type: 'LineString',
              coordinates: coords,
            },
            properties: {
              distanceMeters,
              costingModel: costingModel || 'pedestrian',
            },
          },
          distanceMeters,
          coordinatesBuffer: coordsBuffer,
        };

        const response: WorkerResponseMessage = {
          id,
          type: 'ROUTE_CALCULATE_SUCCESS',
          payload: successPayload,
        };

        // Transfer coordinate buffer to avoid cloning overhead
        self.postMessage(response, [coordsBuffer]);
        break;
      }

      case 'ROUTE_TILE_WRITE_REQUEST': {
        const { filename, tarBuffer } = payload as RouteTileWriteRequestPayload;
        const bytesWritten = await writeRoutingTar(filename, tarBuffer);

        const response: WorkerResponseMessage = {
          id,
          type: 'ROUTE_TILE_WRITE_SUCCESS',
          payload: {
            filename,
            bytesWritten,
          },
        };
        self.postMessage(response);
        break;
      }

      case 'ROUTE_TILE_FETCH_REQUEST': {
        const { bbox } = payload as RouteTileFetchRequestPayload;
        console.log(`[Routing Worker] Fetch requested for bbox: [${bbox.join(', ')}]`);

        // Mock fetch response confirming request acknowledgement
        const response: WorkerResponseMessage = {
          id,
          type: 'SUCCESS',
          payload: {
            message: `Tile fetch initiated for bbox: ${bbox.join(',')}`,
            bbox,
          },
        };
        self.postMessage(response);
        break;
      }

      default: {
        const response: WorkerResponseMessage = {
          id,
          type: 'ERROR',
          payload: null,
          error: `[Routing Worker] Unknown request type: ${type}`,
        };
        self.postMessage(response);
        break;
      }
    }
  } catch (err: unknown) {
    console.error('[Routing Worker] Error processing message:', type, err);
    const message = err instanceof Error ? err.message : String(err);
    const response: WorkerResponseMessage = {
      id,
      type: 'ERROR',
      payload: null,
      error: message || 'Unknown routing worker error',
    };
    self.postMessage(response);
  }
});
