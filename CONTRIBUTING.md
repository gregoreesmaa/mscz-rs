# Contributing to `mscz` (`mscz-rs`)

Thank you for your interest in contributing to `mscz`! We welcome bug reports, notation-feature improvements, round-trip test cases, and documentation fixes.

---

## Prerequisites

- **Rust 1.88+** (MSRV, `edition = 2021`)
- Optional: `wasm32-unknown-unknown` target for verifying browser/WASM compatibility:
  ```bash
  rustup target add wasm32-unknown-unknown
  ```
- Optional: MuseScore 4 (desktop) if you want to visually inspect exported `.mscz` files.

---

## Local Verification Checklist

Before opening a pull request, run:

```bash
# 1. Run the full unit & integration test suite
cargo test

# 2. Check formatting
cargo fmt --check

# 3. Run clippy with warnings denied
cargo clippy --all-targets -- -D warnings

# 4. Verify library-only WASM compilation (no CLI, no filesystem I/O)
cargo check --no-default-features --target wasm32-unknown-unknown
```

---

## Adding a New Notation Feature

`mscz` keeps MusicXML, native MuseScore 4 `.mscx`/`.mscz`, `LossTable`, and `bekern+` consistent by construction. When adding a new musical feature (for example, pedal marks, ottava spanners, or visual `<time-modification>` tuplets), follow this checklist:

1. **Extend the IR** in `src/lib.rs`:
   - Add optional, `#[serde(default)]` fields to `NoteInput` / `MeasureAttr` (or `ScoreInput`) so existing JSON inputs remain backwards-compatible.
   - Propagate the field into `ScoreNote` / `Score` inside `parse_score`.
2. **Render to MusicXML & Native MS4 `.mscx`**:
   - Emit the feature in `to_musicxml` (maintaining valid MusicXML 3.1 partwise structure and exact measure tick sums).
   - Emit the native MS4 equivalent in `to_mscx` (`version="4.20"`). If the feature is a spanner, pair its start/stop endpoints and format `<location>` via `mscx_location_xml` so fractions are GCD-reduced.
3. **Record losses honestly**:
   - If any input edge case cannot be represented faithfully in an output format, push a structured `LossEntry` (`severity = "loss"`) to `LossTable` rather than silently dropping it.
4. **Update `bekern+` / importer (if applicable)**:
   - Wire into `src/musicxml_import.rs`, `src/bekern.rs`, and `src/decode.rs` if the feature belongs in the canonical token vocabulary.
5. **Add round-trip tests**:
   - Verify `validate_notes`, `validate_musicxml`, and `validate_mscz` all return zero errors.
   - Verify `losses.is_lossless()` for lossless cases and assert the exact `LossEntry::kind` for lossy cases.
   - If you changed `.mscx` output, open the generated `.mscz` in MuseScore 4 to confirm it loads cleanly with zero repair warnings.

---

## Standing Rules

- **Zero-GPL policy**: Do not add GPL, LGPL, or AGPL dependencies (Rust crates or external binary invocations) to the library or CLI. All dependencies must be permissive (`MIT`, `Apache-2.0`, `BSD`, `ISC`, `Zlib`, etc.).
- **Determinism**: Outputs must be deterministic for identical inputs (prefer `BTreeMap` / `BTreeSet` over `HashMap` whenever iteration order affects XML or JSON output).
- **WASM safety**: Keep core library functions free of `std::fs` calls; file I/O belongs in `src/main.rs` (the CLI) or explicit helper wrappers.

---

## Licensing

By contributing to `mscz`, you agree that your contributions will be dual-licensed under the **MIT License** and the **Apache License, Version 2.0**, as stated in `LICENSE-MIT` and `LICENSE-APACHE`.
