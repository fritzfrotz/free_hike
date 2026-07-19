// SPDX-License-Identifier: Apache-2.0
// Frontend seam-test runner (P-FE.C1). Node environment only — nothing under
// test touches the DOM; navigator.storage / localStorage are stubbed by the
// harness (src/test/). jsdom is deliberately absent until a test needs it.
import { defineConfig } from 'vitest/config';

export default defineConfig({
  test: {
    environment: 'node',
    include: ['src/**/*.test.ts'],
    setupFiles: ['src/test/setup.ts'],
  },
});
