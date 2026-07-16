import { create } from 'zustand';

/** Filenames (in OPFS) backing the two pmtiles:// sources MapLibre reads from. */
export interface OfflineRegion {
  regionLabel: string;
  basemapFile: string;
  terrainFile: string;
}

/** State machine for the fetch-and-write-to-OPFS pipeline of a new region. */
export type RegionDownloadStatus = 'idle' | 'fetching' | 'writing' | 'done' | 'error';

interface MapState {
  /** The offline region MapLibre's basemap/terrain sources are currently bound to. */
  activeRegion: OfflineRegion | null;
  /** Whether the camera is auto-following the live GPS fix. */
  isTrackingCamera: boolean;
  /** Coarse status of an in-flight region download — cheap to render on. */
  regionDownloadStatus: RegionDownloadStatus;
  /** Human-readable label for the current download step. */
  regionDownloadLabel: string;

  setActiveRegion: (region: OfflineRegion) => void;
  setTrackingCamera: (tracking: boolean) => void;
  setRegionDownloadStatus: (status: RegionDownloadStatus) => void;
  setRegionDownloadLabel: (label: string) => void;
}

/**
 * Map-centric global state: the active offline region and GPS lock status.
 * Deliberately excludes high-frequency data (byte counters, GPS coordinate
 * streams) — those live in refs local to the components that render them,
 * bypassing React/Zustand re-renders entirely (see DownloadProgressBar).
 */
export const useMapStore = create<MapState>((set) => ({
  activeRegion: null,
  isTrackingCamera: false,
  regionDownloadStatus: 'idle',
  regionDownloadLabel: '',

  setActiveRegion: (region) => set({ activeRegion: region }),
  setTrackingCamera: (tracking) => set({ isTrackingCamera: tracking }),
  setRegionDownloadStatus: (status) => set({ regionDownloadStatus: status }),
  setRegionDownloadLabel: (label) => set({ regionDownloadLabel: label }),
}));
