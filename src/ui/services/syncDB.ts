// SPDX-License-Identifier: Apache-2.0
/**
 * syncDB.ts
 *
 * Minimal, promise-wrapped IndexedDB client for the sync manifest.
 *
 * Architecture note (from architecture.md §1):
 *   The dual-tier storage strategy keeps large binary data in OPFS and small
 *   structured metadata in IndexedDB.  This module is exclusively responsible
 *   for the latter — it stores exactly one SyncManifestRecord at a time
 *   (primary key = 'sync_manifest') and never touches OPFS.
 *
 * Database:     freehike_sync_db  v1
 * Object store: sync_manifest        keyPath: 'id'
 */

import type { SyncManifestRecord } from '../../shared/types';

const DB_NAME    = 'freehike_sync_db';
const DB_VERSION = 1;
const STORE_NAME = 'sync_manifest';

// ─── Internal: open / upgrade ─────────────────────────────────────────────────

function openSyncDB(): Promise<IDBDatabase> {
  return new Promise<IDBDatabase>((resolve, reject) => {
    const request = indexedDB.open(DB_NAME, DB_VERSION);

    request.onupgradeneeded = () => {
      const db = request.result;
      if (!db.objectStoreNames.contains(STORE_NAME)) {
        db.createObjectStore(STORE_NAME, { keyPath: 'id' });
      }
    };

    request.onsuccess = () => resolve(request.result);
    request.onerror   = () => reject(request.error);
  });
}

// ─── Public API ───────────────────────────────────────────────────────────────

/**
 * Writes (or replaces) the single sync manifest record.
 * Called after every successful upload batch.
 */
export async function saveSyncMetadata(record: SyncManifestRecord): Promise<void> {
  const db = await openSyncDB();
  return new Promise<void>((resolve, reject) => {
    const tx    = db.transaction(STORE_NAME, 'readwrite');
    const store = tx.objectStore(STORE_NAME);
    const req   = store.put(record);
    req.onsuccess  = () => resolve();
    req.onerror    = () => reject(req.error);
    tx.oncomplete  = () => db.close();
  });
}

/**
 * Reads the sync manifest record.
 * Returns null if the user has never successfully synced.
 */
export async function loadSyncMetadata(): Promise<SyncManifestRecord | null> {
  const db = await openSyncDB();
  return new Promise<SyncManifestRecord | null>((resolve, reject) => {
    const tx    = db.transaction(STORE_NAME, 'readonly');
    const store = tx.objectStore(STORE_NAME);
    const req   = store.get('sync_manifest');
    req.onsuccess  = () => resolve((req.result as SyncManifestRecord | undefined) ?? null);
    req.onerror    = () => reject(req.error);
    tx.oncomplete  = () => db.close();
  });
}

/**
 * Deletes the sync manifest record.
 * Called on user-initiated disconnect to ensure a clean slate.
 */
export async function clearSyncMetadata(): Promise<void> {
  const db = await openSyncDB();
  return new Promise<void>((resolve, reject) => {
    const tx    = db.transaction(STORE_NAME, 'readwrite');
    const store = tx.objectStore(STORE_NAME);
    const req   = store.delete('sync_manifest');
    req.onsuccess  = () => resolve();
    req.onerror    = () => reject(req.error);
    tx.oncomplete  = () => db.close();
  });
}
