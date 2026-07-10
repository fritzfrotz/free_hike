# Phase 3: Spatial Intelligence & Open Ingestion Pipeline Implementation Plan

This phase implements the live OpenStreetMap (OSM) trail ingestion pipeline and high-performance client-side spatial indexing. It will fetch trails dynamically within a viewport bounding box from the public Overpass API (with proper agent identification and rate-limiting backoffs), index them using a static Hilbert R-Tree (`flatbush`), cache the spatial data in the Origin Private File System (OPFS), and execute real-time nearest-trail checks off-thread.

## User Review Required

> [!IMPORTANT]
> **Overpass API Rate Limits:**
> Public Overpass servers are heavily rate-limited (typically 2 concurrent slots per IP, 10k queries/day).
> 1. We enforce `X-Client-Id: Antigravity-Hiking-App/1.0` and `User-Agent` headers to avoid automated `406` blocks.
> 2. We implement exponential backoff (starting at `1000ms`, doubling on `429` or `504` responses) up to 3 retries.
> 3. Viewport queries are tightly constrained by the map's immediate `{{bbox}}` coordinates in `(south, west, north, east)` format to keep payloads minimal.
> 
> *Do you approve of these rate-limit boundaries and client headers?*

## Proposed Changes

### Shared Communication Layer

#### [MODIFY] [src/shared/types.ts](file:///Users/macbook2025/code/Antigravity/free_hike/src/shared/types.ts)
Adds new RPC message types to the communication contract:
*   `'TRAILS_FETCH_BOUNDS'`: Request to query Overpass for a specific bbox, build the Flatbush index, and save the dataset.
*   `'TRAILS_QUERY_NEAREST'`: Request to find the closest trail coordinate to a given `[lng, lat]` point.
*   `'TRAILS_INDEX_COMPLIANCE'`: Response confirming spatial indexing is complete and successful.
*   `'TRAILS_NEAREST_RESPONSE'`: Response payload containing details of the closest trail (name, type, distance in meters, and coordinates of the closest point).

---

### Ingestion & Index Web Worker

#### [NEW] [src/workers/spatial.worker.ts](file:///Users/macbook2025/code/Antigravity/free_hike/src/workers/spatial.worker.ts)
A dedicated worker thread responsible for heavy spatial operations:
1.  **Overpass Client:** Fetches features inside the bounds via a POST request with the custom client headers.
2.  **Flatbush Indexer:** Iterates over the raw way geometries, computes their bounding boxes, and packs them into a flat R-Tree index.
3.  **OPFS Caching:** Synchronously writes the Flatbush `ArrayBuffer` (`index.data`) to `trails_index.bin` and the raw GeoJSON/way features to `trails_features.json` in the OPFS for caching.
4.  **Transferable Communication:** Transfers a sliced copy of the `ArrayBuffer` back to the main thread while retaining a local copy for quick queries.
5.  **Nearest Proximity Solver:** Handles `TRAILS_QUERY_NEAREST` using Flatbush's `.neighbors(lng, lat, 1)` to find the closest bounding box. Then, computes the exact shortest perpendicular distance (in meters) to any of the line segments of that way, returning details back to the UI.

---

### UI Map Component

#### [MODIFY] [src/ui/components/MapView.tsx](file:///Users/macbook2025/code/Antigravity/free_hike/src/ui/components/MapView.tsx)
Updates the map interface and interactive behaviors:
1.  **Floating Control:** Adds a sleek, absolute-positioned glassmorphism button: `"Scan Viewport for Trails"`.
2.  **Layer Registration:** Mounts a dynamic MapLibre GeoJSON source (`'osm-trails'`) and line layer (`'osm-trails-layer'`) styled with a glowing emerald or teal aesthetic.
3.  **Proximity Tracker:** Listens to MapLibre's `mousemove` event (or click event). On movement:
    *   Throttles/Debounces calls to prevent spamming the thread.
    *   Sends a `TRAILS_QUERY_NEAREST` message to the spatial worker.
    *   Renders the closest trail's name, type, and distance (e.g. `Milford Track (120m away)`) dynamically in the HUD panel.
    *   (Optional) Dynamically renders a point or highlight overlay at the closest point on the trail.

---

## Verification Plan

### Automated / Compiler Checks
1.  Verify the project compiles cleanly using `tsc -b` to make sure there are no TypeScript syntax errors or import issues in either thread target.

### Manual Verification
1.  Open the development server.
2.  Click the `"Scan Viewport for Trails"` button.
3.  Check the browser DevTools Console:
    *   Verify the Overpass QL POST request executes with the `X-Client-Id` header.
    *   Verify the spatial worker parses the OSM JSON elements, initializes Flatbush, writes them to OPFS, and replies with index compliance.
4.  Move the cursor across the map near a trail and confirm the HUD panel updates in real time with the closest trail name and distance in meters.
