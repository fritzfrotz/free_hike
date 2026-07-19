# Contributing to FreeHike

Thanks for your interest! A few things to know before diving in:

## This repo is built agentically

FreeHike is developed by an AI agent under human oversight, governed by
[`agentic_operating_manual.md`](agentic_operating_manual.md). That manual —
not tribal knowledge — defines the workflow: test-first chunks, a
verification ladder, human-in-the-loop gates for irreversible decisions, and
an append-only build log ([`freehike-core/LOOPLOG.md`](freehike-core/LOOPLOG.md)).

[`ARCHITECTURE.md`](ARCHITECTURE.md) is the single source of truth for design
decisions. If a change contradicts it, either the change is wrong or the
document must be amended in the same PR.

## Issues

Issues are very welcome — bug reports, device-specific compile failures,
rendering glitches, and data-quality reports are especially valuable.
Include your device model, OS version, and the region you compiled.

## Pull requests

PRs must pass the **L1 ladder** before review:

```bash
# Rust core
cd freehike-core
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check

# Web
npm run lint
npm run build
```

Two consecutive green runs ("green-locked ×2" in manual terms). PRs that
touch the compiler pipeline should also state which L2 (real-data) tests
were run. Changes to the FFI surface must regenerate the UniFFI bindings in
**all** vendored locations (see the "Generated bindings" note in
`ARCHITECTURE.md`).

## License

By contributing you agree that your contributions are licensed under the
[Apache License 2.0](LICENSE).
