// SPDX-License-Identifier: Apache-2.0
/**
 * gpsCamera.ts — the camera moves driven by GPS (P9.C11 addendum).
 *
 * Only eases, never flyTo: flyTo's arc zooms out mid-flight, and below the
 * view clamp's minimum zoom (the archive header's, 14 for compiled regions)
 * the move cannot complete — on RC1 the camera ended pinned at the region
 * bounds' north-west corner while the dot was correct, and tracking
 * restarted the move on every fix. An ease interpolates zoom between its
 * two endpoints, so it never drops below either.
 */

export interface GpsFix {
  lng: number;
  lat: number;
}

/** The map method this needs (a subset of maplibregl.Map). */
export interface EasingMap {
  easeTo(options: { center: [number, number]; zoom?: number; essential?: boolean }): unknown;
}

/** Tracking: pan to the fix at the current zoom. */
export function followFix(map: EasingMap, fix: GpsFix): void {
  map.easeTo({ center: [fix.lng, fix.lat], essential: true });
}

/** The tap: pan to the fix and ease to `zoom`. */
export function centerOnFix(map: EasingMap, fix: GpsFix, zoom: number): void {
  map.easeTo({ center: [fix.lng, fix.lat], zoom, essential: true });
}
