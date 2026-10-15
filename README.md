# FreeHike

**FreeHike is a hiking map app with no server: your phone downloads raw OpenStreetMap data and compiles its own map, then shows it with no network.**

## Quickstart (Android)

1. Download `app-release.apk` from the [v0.1.0 release](https://github.com/fritzfrotz/free_hike/releases/tag/v0.1.0).
2. Install it. If you had an earlier FreeHike test build, uninstall it first (different signer); this deletes its region.
3. Tap **New Region** → **1 · Download region data** — the Portugal extract, about 425 MB. Use Wi-Fi.
4. Put the phone on a charger and tap **2 · Compile "Portugal" in Background** — 9–14 minutes on a Samsung S23 Ultra.
5. When **Region ready** shows, browse the Lisbon area at street level.

The release APK is signed with certificate SHA-256 `cf030fdc886ce4618f9dad04033e9e0cb46ab5e178ff204dccbd998164e72da6` (`apksigner verify --print-certs app-release.apk`). The build's minimum is Android 7.0 (minSdk 24); it has been tested only on Android 16.

## What v0.1 is

**One region, street level.** v0.1 compiles one region, Portugal, from Geofabrik's daily extract. The compiler builds zoom-14 tiles only. The map shows zoom 14, enlarges it above that, and doesn't zoom out below 14. A compile tiles the whole downloaded extract (28,354 tiles for Portugal), while the view stays clamped to the Lisbon box. Coarser zooms are a candidate for after v0.1, not a commitment.

**Background compile (Android).** Android compiles while charging. Started from the app, it runs as a foreground service; progress shows in the app only — on Android 13 and later no notification appears. After a phone reboot mid-compile, Android restarted the compile in the background on its own, without the app being opened, and it ran to completion (release build, Samsung S23 Ultra, Android 16, airplane mode, on USB power). The finished map was handed to the app on the next open.

**Offline.** On 2026-10-12 the whole compile ran in airplane mode: about 8 min 51 s, 28,354 tiles, a 94 MB archive. The build doesn't log successful network requests. So "no network use" rests on three things: no errors, no non-local URLs, and no traffic recorded for the app during the compile. It does not rest on a packet capture. GPS fixes keep arriving in airplane mode.

**Interruptions.** Every pass writes its checkpoint to disk only after the data it covers. Results on the phone so far:
- **Reboot.** In the 2026-10-14 run (RC2) the compile resumed from its checkpoint after the restart and finished after the app was opened; the app's location service kept that run alive (D017). In the 2026-10-15 release run, a reboot at 50 % with the app in the background resumed by itself and ran to 100 % without the app being opened.
- **Kill mid-download (RC2).** The download finished complete and verified and was never restarted from zero. No record shows directly that the bytes already on disk were reused.
- **Kill mid-compile (RC2).** Passed on the operator's notes only; no log of it survived.
- **Desktop kill-test harness.** The SIGKILL torture harness the operating manual calls for was never built; in-process resume tests stand in (D010).

**Heat.** When the phone runs hot, the compile shortens its slices and pauses to cool. In RC2 this looked like progress stuck near 51 % for about three minutes after a reboot.

**Memory.** The engine is designed to a 50 MB dirty-heap budget. On the phone that budget is measured only by a proxy:
- During RC2 (debug build) the app's native heap peaked at about 34 MB.
- The whole app process, web view included, peaked at about 136 MB.
- The engine reports no figure of its own (D009). The release build logs no memory figures.

## Tested on

| Run | Build | Device | Result |
|---|---|---|---|
| 2026-10-12 | debug, main `8c3a3ed` | Samsung S23 Ultra, Android 16 | fetch, airplane-mode compile, handoff on second attempt |
| 2026-10-14 (RC2) | debug, main `58190b2` | Samsung S23 Ultra, Android 16 | download kill, two reboots mid-compile, finish, map drawn, GPS follow |
| 2026-10-15 | release 0.1.0 (100), main `c7ced7e` | Samsung S23 Ultra, Android 16 | fetch, airplane-mode compile, two reboots mid-compile (the second headless to 100 %), map on open, GPS, force-stop and reopen |

## Not in v0.1

- **Routing.** Not in v0.1. The map came first.
- **Terrain** (hillshade, contours). Cut. The terrain crate stays in the tree, unused. The real terrain path is deferred, not dead: after v0.1 ships, ranked ahead of routing.
- **iOS.** The iOS shell is in the tree, but this project's environment has no Xcode, so it has never been compiled or run here. Unverified (D004). There is no iOS build.
- **Zoom-out below street level.** See above. Coarse zooms are a candidate, not a commitment.
- **A compile notification.** None on Android 13+ (D018).

## Known issues

- After the compile, the archive is copied into app storage through the web view. A low-memory kill can interrupt the copy; it finishes on the next app open (D011).
- The compile tiles the whole extract, not only the selected box (D015).
- A compile started at boot gets no foreground service on Android 15+. This is inferred, not observed (D016).
- The map's GPS watcher holds a location foreground service for the app's whole life on Android. In the RC2 run that service kept the post-reboot compile alive (D017).
- In the release build the Rust compiler still writes about five lines per compile slice to the system log (D019).
- Sprite and glyph gaps in the map style (B008).

The full list is [`TRACKER.md`](TRACKER.md).

## How it works

```
Geofabrik extract ──▶ raw .osm.pbf on the phone      (resumable, verified download)
      │
      ▼
Native Rust compiler (freehike-core)                  design budget: < 50 MB dirty heap
  Pass 1  stream nodes → on-flash B-tree (redb)
  Pass 2  stream ways  → geometry + tags
  Pass 3  simplify, clip, bin into z14 tiles
  Finalize MVT → gzip → dedup → PMTiles v3           (sequential writes)
      │
      ▼
copy into OPFS ──▶ MapLibre GL draws it, no network
```

Rust runs natively on the phone, not as WebAssembly. The large files never cross the bridge to the web view: a region request goes down, and progress events come up. The archive copy is the one exception (D011). Hiking paths carry their SAC grade (T1–T6) and names into the tiles.

| Layer | Technology |
|---|---|
| Map compiler | Rust: mmap + streaming PBF parse, redb, PMTiles v3 writer |
| Bridge | UniFFI → Kotlin (Swift generated, unverified — D004), Capacitor plugin |
| Background | Android WorkManager + foreground service |
| UI | React + Vite + Zustand in Capacitor; MapLibre GL JS over OPFS |

## Why compile on the phone?

The constraint is zero infrastructure cost: no tile server, no routing API, no account database. A download from a CDN beats a compile on the phone whenever a server exists. FreeHike has none by design, so the phone has to build its own map. [`ARCHITECTURE.md`](ARCHITECTURE.md) has the full rationale.

## How it's built

FreeHike is written by an AI coding agent under the control of a human operator:
- Work is cut into test-first chunks, and each chunk names its proofs before any code is written.
- Plans and close reports are reviewed by a separate Claude session working with the operator. A review by a model from a second vendor is not in place.
- Tests, CI and a tracker janitor check the work mechanically.
- The operator approves dependencies, architecture changes and every commit.

Load-bearing decisions go into a [decision ledger](docs/decision-ledger.md) with their reasons. Every chunk and device run is recorded in an append-only build log, [`freehike-core/LOOPLOG.md`](freehike-core/LOOPLOG.md). The rules are in [`agentic_operating_manual.md`](agentic_operating_manual.md).

## Building from source

```bash
npm install && npm run build && npx cap sync android
cd freehike-core && cargo test && cargo clippy --all-targets -- -D warnings
cargo ndk -t arm64-v8a -o ../android/app/src/main/jniLibs build --release -p ffi
```

Debug builds need nothing else. `assembleRelease` needs the maintainer's signing files and fails without them. `npm install` turns on the pre-commit check (tracker janitor plus a privacy pass). Desktop test fixtures can be rebuilt with `scripts/compile_sandbox_data.sh` (see its header).

## Data & attribution

Map data © [OpenStreetMap](https://www.openstreetmap.org/copyright) contributors, ODbL. Fonts: Noto Sans (SIL OFL).

## License

[Apache-2.0](LICENSE).
