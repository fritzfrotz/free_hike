# Tracker Tags — Inline Debt/Bug/Exemption Standard

One grep-able comment standard across Rust, Swift, Kotlin, and TypeScript.
Enforced mechanically by `scripts/tracker-janitor.mjs` (pre-commit + CI).
Authority: the **code** is the source of truth for open debt/bugs;
`TRACKER.md` is a generated, machine-owned view (never hand-edited);
`ARCHITECTURE.md` owns rules; `freehike-core/LOOPLOG.md` is the only
graveyard for fixed items.

## The three tags

```
// DEBT(D001): <one-line description> — platforms: <ios,android,web,core>
// BUG(B001): <one-line description> — severity: <blocker|major|minor> — repro: <one line or LOOPLOG ref>
// RULE-EXEMPT(P8a): <justification for a sanctioned exception to a mechanical rule>
```

- The comment marker adapts per language (`//` for Rust/Swift/Kotlin/TS/JS,
  `#` for shell/Python/TOML/YAML); the `TAG(ID):` core is identical everywhere.
- The field separator is an em dash `—` (a double hyphen `--` is also accepted).

## ID rules

- **Stable**: an ID never changes meaning; once assigned it refers to that
  item forever (including after it is fixed and buried in LOOPLOG).
- **Unique**: one item, one ID. The janitor fails the build if the same ID
  appears with conflicting descriptions/metadata.
- **Monotonic**: take the next free number (`D###`/`B###`, three or more
  digits, zero-padded). Check `TRACKER.md` for the current high-water mark.
- **Cross-platform items are ONE ID tagged at multiple sites** — a debt that
  lives on iOS and Android carries the same `D###` in each platform tree.
  The `platforms:` list is the contract: the janitor fails if a declared
  platform's tree has no site for that ID (the mirror check).

Platform → tree mapping used by the mirror check:

| platform | tree |
|---|---|
| `ios` | `ios/` |
| `android` | `android/` |
| `web` | `src/` |
| `core` | `freehike-core/` |

## Severity (BUG only)

- `blocker` — breaks a shipped flow or corrupts data; treated as
  chunk-planning input at session start (operating manual).
- `major` — wrong behavior with a workaround.
- `minor` — cosmetic/nit.

`repro:` is one line, or a pointer like `repro: LOOPLOG P9.C4`.

## RULE-EXEMPT

`RULE-EXEMPT(<rule-id>): <justification>` marks a sanctioned exception to a
mechanical rule declared in `ARCHITECTURE.md` (fenced `rule-id:` /
`forbidden-pattern:` / `paths:` blocks). The exemption suppresses a
forbidden-pattern match **on the same line or the immediately following
line** — put the tag on its own comment line directly above the offending
line, or at the end of it. Every exemption is listed in `TRACKER.md`;
an exemption without a justification is malformed and fails the check.

## Lifecycle

1. **Open** — tag the code at the site(s) where the debt/bug actually lives.
2. **Tracked** — `tracker-janitor.mjs --fix` regenerates `TRACKER.md`
   (run at session close; committed with the session's work).
3. **Fixed** — remove the tag(s) in the fixing commit AND append a LOOPLOG
   entry containing `closes D###` / `closes B###`. An ID that disappears
   from code without a `closes` line in LOOPLOG is flagged by the janitor
   ("resolved but not buried") until buried.

## Examples

```rust
// DEBT(D003): PMTiles writer is root-directory-only; leaf splitting needed at scale — platforms: core
```

```kotlin
// BUG(B002): FGS promotion silently refused on API 31+ cold start — severity: minor — repro: LOOPLOG P8.C3
```

```typescript
// RULE-EXEMPT(P8a): initial style load — the one sanctioned setStyle call
map.setStyle(baseStyle);
```
