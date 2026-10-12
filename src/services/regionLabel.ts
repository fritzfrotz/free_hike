// SPDX-License-Identifier: Apache-2.0
/**
 * regionLabel.ts — the name the demo screen shows for a bound archive
 * (P9.C10).
 *
 * A compiled region is named by its job ID, `bg_<slug>_<base36 time>`
 * (regionCompiler.enqueueRegionDownload), and its OPFS archive is
 * `<jobId>.pmtiles`. The slug is the only readable name the device keeps,
 * so it is turned back into words here — lossy for labels that had
 * punctuation, fine for v0.1's one region ("Portugal"). Anything else
 * (the dev preview archive, a debug job) is shown as given.
 */

const JOB_ID = /^bg_([a-z0-9-]+)_[a-z0-9]+$/;

/** "bg_portugal_m3x9k2(.pmtiles)" → "Portugal"; otherwise the name itself. */
export function regionDisplayName(fileOrJobId: string): string {
  const id = fileOrJobId.replace(/\.pmtiles$/, '');
  const match = JOB_ID.exec(id);
  if (!match) return id;
  return match[1]
    .split('-')
    .filter(Boolean)
    .map((word) => word.charAt(0).toUpperCase() + word.slice(1))
    .join(' ');
}

/** Banner text when a saved region's archive is gone from OPFS at boot. */
export function savedRegionMissingMessage(fileOrJobId: string): string {
  return `The offline map for ${regionDisplayName(fileOrJobId)} is no longer on this device — download it again.`;
}
