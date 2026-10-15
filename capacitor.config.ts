import type { CapacitorConfig } from '@capacitor/cli';

const config: CapacitorConfig = {
  appId: 'com.freehike.app',
  appName: 'FreeHike',
  webDir: 'dist',
  // Web console lines reach logcat only in debuggable builds (P10.C1):
  // a release APK carries no per-second GPS line. Capacitor's default,
  // pinned by src/test/releaseLogging.test.ts.
  loggingBehavior: 'debug',
};

export default config;
