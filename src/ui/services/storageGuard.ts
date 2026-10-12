// SPDX-License-Identifier: Apache-2.0
/**
 * storageGuard.ts
 *
 * Hardens local storage persistence against WebKit/OS silent eviction.
 * Requests durable storage permission and estimates remaining disk space.
 */

export interface StorageStatus {
  isPersistent: boolean;
  quotaGb: number;
  usageGb: number;
}

/**
 * P9.C10 — whether to show the "storage is not persistent" warning. Never on
 * native: Android's web view answered persist() === false on every boot of
 * the device runs and has no way to grant it, so the warning was noise the
 * user could not act on. An eviction that actually happens is announced
 * instead (the saved region's file is gone at boot — MapView's
 * onSavedRegionMissing). `isPersistent` null = not answered yet.
 */
export function showStorageWarning(isNative: boolean, isPersistent: boolean | null): boolean {
  return !isNative && isPersistent === false;
}

/**
 * Requests that the browser retain storage durably (preventing automatic OS eviction)
 * and estimates the remaining storage quota in Gigabytes.
 */
export async function requestPersistentStorage(): Promise<StorageStatus> {
  const status: StorageStatus = {
    isPersistent: false,
    quotaGb: 0,
    usageGb: 0,
  };

  if (typeof navigator !== 'undefined' && navigator.storage) {
    try {
      // 1. Request persistence (if permission is granted, isPersistent will be true)
      if (navigator.storage.persist) {
        status.isPersistent = await navigator.storage.persist();
      } else if (navigator.storage.persisted) {
        // Fallback check if request API isn't present but check is
        status.isPersistent = await navigator.storage.persisted();
      }

      // 2. Query quota estimate
      if (navigator.storage.estimate) {
        const estimate = await navigator.storage.estimate();
        const usageBytes = estimate.usage || 0;
        const quotaBytes = estimate.quota || 1; // default to avoid division issues

        status.usageGb = parseFloat((usageBytes / (1024 * 1024 * 1024)).toFixed(3));
        status.quotaGb = parseFloat((quotaBytes / (1024 * 1024 * 1024)).toFixed(3));
      }

      // 3. Log results to console
      if (status.isPersistent) {
        console.log(
          `[StorageGuard] Success: Storage is persistent. ` +
          `Quota: ${status.quotaGb} GB | Usage: ${status.usageGb} GB`
        );
      } else {
        console.warn(
          "WARNING: Storage is not persistent. OPFS map data is at risk of silent OS eviction."
        );
      }
    } catch (err) {
      console.error('[StorageGuard] Failed to request or estimate persistent storage:', err);
    }
  } else {
    console.warn('[StorageGuard] navigator.storage API is not supported in this browser.');
  }

  return status;
}
