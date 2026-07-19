// SPDX-License-Identifier: Apache-2.0
/**
 * db.ts
 *
 * Lightweight, promise-based IndexedDB wrapper for route state management.
 * Avoids any external dependencies (e.g. idb) to maintain zero-overhead principles.
 *
 * Database Name: FreeHikeDB
 * Store Name:    saved_routes (Key path: 'id', autoIncrement: true)
 */

export interface SavedRoute {
  id?: number;
  title: string;
  timestamp: number;
  /** Flat coordinates buffer [lng0, lat0, lng1, lat1, ...] */
  coordinates: Float64Array;
  totalAscent: number;
  totalDescent: number;
  /** Flat elevation profile values */
  elevations: Float64Array;
}

const DB_NAME = 'FreeHikeDB';
const DB_VERSION = 1;
const STORE_NAME = 'saved_routes';

function openDB(): Promise<IDBDatabase> {
  return new Promise<IDBDatabase>((resolve, reject) => {
    const request = indexedDB.open(DB_NAME, DB_VERSION);

    request.onupgradeneeded = () => {
      const db = request.result;
      if (!db.objectStoreNames.contains(STORE_NAME)) {
        db.createObjectStore(STORE_NAME, { keyPath: 'id', autoIncrement: true });
      }
    };

    request.onsuccess = () => resolve(request.result);
    request.onerror   = () => reject(request.error);
  });
}

/**
 * Saves a route to the database.
 * If the route already has an 'id', it will be updated; otherwise, a new one is created.
 */
export async function saveRoute(routeData: SavedRoute): Promise<number> {
  const db = await openDB();
  return new Promise<number>((resolve, reject) => {
    const tx    = db.transaction(STORE_NAME, 'readwrite');
    const store = tx.objectStore(STORE_NAME);
    const req   = store.put(routeData);

    req.onsuccess = () => resolve(req.result as number);
    req.onerror   = () => reject(req.error);
    tx.oncomplete = () => db.close();
  });
}

/**
 * Retrieves all saved routes from the database.
 */
export async function getAllRoutes(): Promise<SavedRoute[]> {
  const db = await openDB();
  return new Promise<SavedRoute[]>((resolve, reject) => {
    const tx    = db.transaction(STORE_NAME, 'readonly');
    const store = tx.objectStore(STORE_NAME);
    const req   = store.getAll();

    req.onsuccess = () => resolve((req.result as SavedRoute[]) || []);
    req.onerror   = () => reject(req.error);
    tx.oncomplete = () => db.close();
  });
}

/**
 * Deletes a saved route by its ID.
 */
export async function deleteRoute(id: number): Promise<void> {
  const db = await openDB();
  return new Promise<void>((resolve, reject) => {
    const tx    = db.transaction(STORE_NAME, 'readwrite');
    const store = tx.objectStore(STORE_NAME);
    const req   = store.delete(id);

    req.onsuccess = () => resolve();
    req.onerror   = () => reject(req.error);
    tx.oncomplete = () => db.close();
  });
}
