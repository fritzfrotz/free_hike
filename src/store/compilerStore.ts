import { create } from 'zustand';

interface CompilerState {
  /** Whether a native Rust compile job is currently running (or resuming). */
  isCompiling: boolean;
  /** Human-readable phase label, e.g. "pass1: indexing nodes (12/62)". */
  currentPhase: string;
  /** Set when the native layer reports the compile thread pool was throttled. */
  thermalThrottling: boolean;

  setCompiling: (isCompiling: boolean) => void;
  setPhase: (phase: string) => void;
  setThermalThrottling: (throttling: boolean) => void;
}

/**
 * Low-frequency compilation state (phase transitions, job status). Per-block
 * byte/percentage telemetry is intentionally excluded — that data streams at
 * 50-100 events/sec from the native bridge and must bypass React state
 * entirely (rAF + ref) to avoid VDOM thrashing.
 */
export const useCompilerStore = create<CompilerState>((set) => ({
  isCompiling: false,
  currentPhase: '',
  thermalThrottling: false,

  setCompiling: (isCompiling) => set({ isCompiling }),
  setPhase: (currentPhase) => set({ currentPhase }),
  setThermalThrottling: (thermalThrottling) => set({ thermalThrottling }),
}));
