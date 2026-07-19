// SPDX-License-Identifier: Apache-2.0
/**
 * resetStores.ts — fresh Zustand state per test (P-FE.C1 harness).
 *
 * The app's stores are module-level singletons; a create-factory refactor
 * is out of scope (no decomposition). Instead: snapshot each store's
 * initial state once at import time and replace wholesale (`setState(_,
 * true)`) in beforeEach. The snapshots contain the bound action closures,
 * so behavior is identical after every reset.
 *
 * NOTE: importing this module pulls in compilerStore → plugins/MapCompiler
 * → @capacitor/*; suites that use it must vi.mock those modules first.
 */
import { useCompilerStore } from '../store/compilerStore';
import { useMapStore } from '../store/mapStore';

const compilerSnapshot = { ...useCompilerStore.getState() };
const mapSnapshot = { ...useMapStore.getState() };

export function resetStores(): void {
  useCompilerStore.setState({ ...compilerSnapshot }, true);
  useMapStore.setState({ ...mapSnapshot }, true);
}
