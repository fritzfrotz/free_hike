// SPDX-License-Identifier: Apache-2.0
/**
 * locationTracker.ts
 *
 * Abstracts GPS tracking behind a single startTracking / stopTracking interface.
 *
 * Strategy:
 *   • Native (iOS / Android via Capacitor) → @capacitor-community/background-geolocation
 *     Runs even when the screen is locked or the app is backgrounded, using the
 *     OS's native background location API.  On Android a persistent foreground-
 *     service notification is shown; on iOS the "Always" location permission is
 *     requested automatically.
 *
 *   • Web (desktop browser / Vite dev server) → navigator.geolocation.watchPosition
 *     Standard W3C API, sufficient for development and PWA installs where the
 *     screen-lock pause is acceptable.
 *
 * The two runtimes use different watcher-ID types:
 *   • Native  → string  (returned by BackgroundGeolocation.addWatcher)
 *   • Web     → number  (returned by navigator.geolocation.watchPosition)
 *
 * Both types are captured in the TrackerHandle union so callers can pass the
 * opaque handle straight back to stopTracking() without caring which path ran.
 */

import { registerPlugin, Capacitor } from '@capacitor/core';
import type { BackgroundGeolocationPlugin, Location } from '@capacitor-community/background-geolocation';

// ---------------------------------------------------------------------------
// Plugin registration
// ---------------------------------------------------------------------------

/**
 * registerPlugin lazily bridges to the native layer when running on iOS/Android.
 * On the web it returns a stub with no-op implementations — we guard every
 * native call with Capacitor.isNativePlatform() so the stub is never invoked.
 */
const BackgroundGeolocation = registerPlugin<BackgroundGeolocationPlugin>(
  'BackgroundGeolocation'
);

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/** Normalised position delivered to every caller regardless of runtime. */
export interface GpsPosition {
  lng: number;
  lat: number;
  /** Horizontal accuracy radius in metres (68 % confidence). */
  accuracy: number;
}

/**
 * Opaque handle returned by startTracking().
 * Pass it unchanged to stopTracking() — the implementation selects the
 * correct cleanup path based on the discriminant tag.
 */
export type TrackerHandle =
  | { kind: 'native'; id: string }
  | { kind: 'web'; id: number };

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/**
 * Begin receiving GPS fixes.
 *
 * @param onPosition  Called with each new fix.
 * @param onError     Called on permission denial or hardware errors.
 * @returns           An opaque TrackerHandle to pass to stopTracking().
 */
export async function startTracking(
  onPosition: (pos: GpsPosition) => void,
  onError?: (err: Error) => void
): Promise<TrackerHandle> {

  // ── Native path (iOS / Android) ────────────────────────────────────────────
  if (Capacitor.isNativePlatform()) {
    const id = await BackgroundGeolocation.addWatcher(
      {
        // Show a persistent notification so Android allows background updates.
        backgroundMessage:  'Tracking your hike',
        backgroundTitle:    'FreeHike Navigation',
        // Prompt for permission automatically if not yet granted.
        requestPermissions: true,
        // Reject stale cached fixes — we only want live sensor data.
        stale:              false,
        // Minimum distance (metres) between delivered fixes.
        // 0 = deliver every fix; increase to reduce battery drain on long hikes.
        distanceFilter:     0,
      },
      (position?: Location, error?: Error) => {
        if (error) {
          console.warn('[LocationTracker] Native GPS error:', error);
          onError?.(error);
          return;
        }
        if (position) {
          onPosition({
            lng:      position.longitude,
            lat:      position.latitude,
            accuracy: position.accuracy,
          });
        }
      }
    );

    console.log(`[LocationTracker] Native background watcher started (id: ${id}).`);
    return { kind: 'native', id };
  }

  // ── Web fallback (desktop browser / PWA) ───────────────────────────────────
  if (!navigator.geolocation) {
    throw new Error('[LocationTracker] Geolocation API is not available in this browser.');
  }

  const id = navigator.geolocation.watchPosition(
    (position) => {
      const { longitude: lng, latitude: lat, accuracy } = position.coords;
      onPosition({ lng, lat, accuracy });
    },
    (err) => {
      console.warn('[LocationTracker] Web GPS error:', err.message);
      onError?.(new Error(err.message));
    },
    { enableHighAccuracy: true, timeout: 5_000, maximumAge: 0 }
  );

  console.log(`[LocationTracker] Web watchPosition started (id: ${id}).`);
  return { kind: 'web', id };
}

/**
 * Stop receiving GPS fixes and release all associated OS resources.
 *
 * @param handle  The TrackerHandle returned by the matching startTracking() call.
 */
export async function stopTracking(handle: TrackerHandle | null): Promise<void> {
  if (!handle) return;

  if (handle.kind === 'native') {
    await BackgroundGeolocation.removeWatcher({ id: handle.id });
    console.log(`[LocationTracker] Native watcher removed (id: ${handle.id}).`);
  } else {
    navigator.geolocation.clearWatch(handle.id);
    console.log(`[LocationTracker] Web watcher cleared (id: ${handle.id}).`);
  }
}
