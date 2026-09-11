// SPDX-License-Identifier: Apache-2.0
import { create } from 'zustand';
import { persist } from 'zustand/middleware';

/** Filenames (in OPFS) backing the two pmtiles:// sources MapLibre reads from. */
export interface OfflineRegion {
  regionLabel: string;
  basemapFile: string;
  terrainFile: string;
}

interface MapState {
  /** The offline region MapLibre's basemap/terrain sources are currently bound to. */
  activeRegion: OfflineRegion | null;
  /** Whether the camera is auto-following the live GPS fix. */
  isTrackingCamera: boolean;
  /** P9.C3 — true while the map is in custom-region selection mode (fixed
   *  reticle overlay; user pans/zooms the map beneath it). Lives here, not
   *  in component state, because RegionPicker (App) enters the mode and
   *  RegionSelectorOverlay (MapView) exits it. NOT persisted (partialize). */
  isSelectingRegion: boolean;

  setActiveRegion: (region: OfflineRegion) => void;
  /** Drops the persisted region (e.g. its OPFS files were evicted) so the
   *  next cold boot falls back to the style's default archives. */
  clearActiveRegion: () => void;
  setTrackingCamera: (tracking: boolean) => void;
  setSelectingRegion: (selecting: boolean) => void;
}

/**
 * Map-centric global state: the active offline region and GPS lock status.
 * Deliberately excludes high-frequency data (byte counters, GPS coordinate
 * streams) — those live in refs local to the components that render them,
 * bypassing React/Zustand re-renders entirely (see BackgroundHandoffBar).
 *
 * P9.C2 — `activeRegion` (and only it, see partialize) survives app
 * restarts via zustand/persist over localStorage: a background-compiled
 * region stays the active map across cold boots instead of resetting to the
 * style's default archives. localStorage rather than Capacitor Preferences
 * because rehydration is SYNCHRONOUS — the persisted region is already in
 * the store before MapView's first render, so the boot path needs no async
 * hydration gate. (In a Capacitor shell, WebView localStorage lives in the
 * app's own data directory — same durability class as OPFS itself; and
 * MapView's cold-boot replay verifies the OPFS files still exist before
 * binding to them anyway.)
 */
export const useMapStore = create<MapState>()(
  persist(
    (set) => ({
      activeRegion: null,
      isTrackingCamera: false,
      isSelectingRegion: false,

      setActiveRegion: (region) => set({ activeRegion: region }),
      clearActiveRegion: () => set({ activeRegion: null }),
      setTrackingCamera: (tracking) => set({ isTrackingCamera: tracking }),
      setSelectingRegion: (selecting) => set({ isSelectingRegion: selecting }),
    }),
    {
      name: 'freehike-active-region',
      // Transient UI state (camera lock, selection mode) must NOT be
      // resurrected on boot — only the region binding is durable.
      partialize: (s) => ({ activeRegion: s.activeRegion }),
    },
  ),
);
