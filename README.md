# FreeHike

A local-first, offline-capable hiking navigation app built with Vite + React, MapLibre GL, and Capacitor.

## What it does

- Renders offline vector maps from `.pmtiles` archives stored in the browser's Origin Private File System (OPFS)
- Tracks your position in real time using native background GPS on iOS/Android (via Capacitor) with a web API fallback for desktop
- Calculates hiking routes using a Valhalla WASM routing engine running entirely on-device — no server required
- Displays trail networks parsed from OpenStreetMap and stored locally as binary FlatGeobuf (`.fgb`) files
- Exports recorded tracks as GPX files

## Tech stack

| Layer | Technology |
|---|---|
| UI | React + Vite |
| Maps | MapLibre GL JS + PMTiles |
| Routing | Valhalla WASM (`@jansoft/mbujkanji-valhalla-wasm`) |
| Storage | OPFS (Origin Private File System) |
| Native wrapper | Capacitor (iOS + Android) |
| Background GPS | `@capacitor-community/background-geolocation` |
| Trail data format | FlatGeobuf (`.fgb`) |

## Getting started

```bash
# Install dependencies
npm install

# Run the dev server
npm run dev

# Production build
npm run build
```

## Mobile (iOS / Android)

```bash
# Build the web app first
npm run build

# Sync into the native projects
npx cap sync

# Open in Xcode
npx cap open ios

# Open in Android Studio
npx cap open android
```

> **iOS note:** Add `NSLocationAlwaysAndWhenInUseUsageDescription` and `NSLocationWhenInUseUsageDescription` to `ios/App/App/Info.plist` before submitting to the App Store.

## Architecture notes

- All compute (routing, tile parsing, spatial indexing) runs in Web Workers to keep the main thread free.
- OPFS is used for durable, high-performance binary storage. The app requests `navigator.storage.persist()` on startup to prevent OS eviction on low-disk devices.
- The Valhalla WASM router is capped at 512 MB of linear memory with an OOM recovery loop — if a route exceeds the limit the worker resets cleanly and returns a user-friendly error.
