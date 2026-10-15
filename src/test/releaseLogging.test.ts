// SPDX-License-Identifier: Apache-2.0
/**
 * releaseLogging.test.ts — P10.C1: a release APK carries no web console line
 * in logcat (the per-second GPS fix, easeTo and map-idle lines included).
 *
 * Capacitor forwards console.* to logcat only through its native Logger.
 * With loggingBehavior 'debug' the Logger is on only when the APK is
 * debuggable — the same flag the Kotlin side reads (DebugBuild.kt). The
 * value is Capacitor's default; pinned here so an edit cannot flip it to
 * 'production' unnoticed.
 */
import { describe, expect, it } from 'vitest';
import config from '../../capacitor.config';

describe('P10.C1 release logging', () => {
  it("Capacitor logs to logcat in debuggable builds only ('debug')", () => {
    expect(config.loggingBehavior).toBe('debug');
    expect(config.android?.loggingBehavior).toBeUndefined();
  });
});
