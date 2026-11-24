# AGENTS.md — `mscz` (`mscz-rs`)

Pure-Rust, zero-GPL symbolic music notation library and CLI: JSON note lists, MusicXML 3.1 (4.0 opt-in), native MuseScore 4 (`.mscx` / `.mscz`, `version="4.20"`), and canonical `bekern+` token streams — all converging on a single deterministic 480-tick IR (`Score`) with an explicit `LossTable` and three-stage validators.

- **Crate / Binary:** `mscz` (`v0.1.0`, live on GitHub `https://github.com/mscz-rs/mscz-rs` and `crates.io`)
- **Toolchain:** Rust `1.88+` (MSRV), `edition = "2021"`
- **License:** `MIT OR Apache-2.0`

## Layout

| Path | Responsibility |
|---|---|
| `src/lib.rs` | Core 480-tick IR (`SCORE_DIVISIONS`, `ScoreInput`, `Score`, `Part`, `ChordGroup`, `ScoreNote`, `MeasureAttr`), `parse_score`, `LossTable` / `LossEntry`, MusicXML 3.1/4.0 renderer (`to_musicxml`, `to_musicxml_version`), native MS4 renderer (`to_mscx`), in-memory `.mscz` zip packer/reader (`pack_mscz`, `pack_mscz_bytes`, `read_mscz`), validators (`validate_notes`, `validate_musicxml`, `validate_mscz`), and SER (`levenshtein`, `ser_for_inputs`, `ser_for_symbols`). |
| `src/builder.rs` | Fluent `ScoreBuilder` and `PartBuilder` APIs for ergonomic Rust score construction. |
| `src/pitch.rs` | Scientific pitch parser (`C4`, `F#5`, `Bb3`, `Gx2`, microtone cents `A4+30c`), MIDI conversion, enharmonic `(step, alter, octave)` preservation, and Tonal Pitch Class (`tpc`) helpers. |
| `src/musicxml_import.rs` | MusicXML 3.1 partwise importer → `ScoreInput` (whole score or paginated via `<print new-page="yes">`), `bekern_events`, `system_breaks`, `page_breaks`, `staff_headers*`, and `split_measures`. |
| `src/bekern.rs` | Feature-gated (`bekern`). Canonical `bekern+` encoder (`to_bekern`) supporting headers, voices, chords, authoritative `<time-modification>` tuplets, grace notes, slurs, ties, dynamics, hairpins, and barlines. |
| `src/decode.rs` | Feature-gated (`bekern`). `bekern+` decoder: `decode_bekern` (merged tie chains for note-list scoring) and `decode_bekern_unmerged` (link-faithful ties for MusicXML/MS4 export). |
| `src/lattice.rs` | Feature-gated (`lattice`). Measure-atomic top-2 candidate lattice sidecar validator and ranker (`cluster_size * (1.0 - confidence)`). |
| `src/main.rs` | Feature-gated (`cli`). Thin `clap` CLI exposing all 12 subcommands (`example`, `serialize`, `mscz`, `pack`, `validate`, `import`, `bekern`, `bekern-decode`, `bekern2musicxml`, `split`, `system-breaks`, `ser`). |
| `examples/` | Runnable end-to-end examples: `01_compose_two_voice.rs`, `02_musicxml_to_mscz.rs`, `03_validate_and_inspect.rs`, `04_bekern_ml_roundtrip.rs` (`required-features = ["bekern"]`). |
| `tests/fixtures/` | CC0 1.0 OpenScore Lieder MusicXML 3.1 fixtures (`ds0001.musicxml`, `ds0037.musicxml`, `ds0409.musicxml`) + `tests/integration_public_api.rs`. |
| `.github/workflows/ci.yml` | Authoritative CI matrix (`ubuntu-latest`, `windows-latest`, `macos-latest`): formatting, Clippy, tests (`--all-features` & `--no-default-features`), rustdoc, and `wasm32-unknown-unknown`. |
| `.github/workflows/release.yml` | Automated `crates.io` publish and GitHub Release pipeline triggered by `v*.*.*` tags (or manual `workflow_dispatch` dry-run). |

## Verification Commands

Every change must pass the exact CI matrix before merge or release:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features
cargo test --all-targets --all-features
cargo test --no-default-features
cargo test --doc
cargo check --target wasm32-unknown-unknown --no-default-features
```

> On Windows PowerShell, set `$env:RUSTDOCFLAGS="-D warnings"` before `cargo doc --no-deps --all-features` and clear it afterwards (`Remove-Item Env:RUSTDOCFLAGS`).

## Hard Invariants (Do Not Break)

1. **Zero-GPL permissive stack & pure-Rust WASM safety:**
   - Crate license is `MIT OR Apache-2.0`. Never add GPL, LGPL, or AGPL dependencies, C/C++ FFI, or external binary invocations (no MuseScore binary, Qt, or Ghostscript).
   - Keep `zip = { version = "2", default-features = false, features = ["deflate"] }` so `.mscz` archives build deterministically in pure Rust on `wasm32-unknown-unknown`.
   - Keep the library core free of `std::fs` / disk I/O; operate on `&str` / `&[u8]` / `Cursor<Vec<u8>>`. Filesystem I/O belongs in `src/main.rs` (or explicit CLI/file wrappers).
2. **`480`-tick integer grid, line-of-fifths `tpc`, and GCD-reduced MS4 `<location>` spanners:**
   - All timings resolve immediately in `parse_score` to integer ticks on `SCORE_DIVISIONS = 480` (`round(beats * divisions)`). Measure windows (`Score::measure_windows`) must honor mid-stream `MeasureAttr::time` changes, and every voice must sum to the exact measure tick length.
   - Preserve enharmonic `(step, alter, octave)` separately from `midi` and compute MuseScore `<tpc>` on the line of fifths centered at `C = 14` (`F=13, C=14, G=15, D=16, A=17, E=18, B=19`; `tpc(step, alter) = base(step) + alter * 7`).
   - All MuseScore 4 (`version="4.20"`) cross-note / cross-bar spanners (`Tie`, `Slur`, `HairPin`) must emit paired `<next><location>` / `<prev><location>` via `mscx_location_xml` with fractions relative to one whole note (`4 * divisions`) reduced by `gcd(|df_ticks|, whole)`.
3. **MusicXML 3.1 DTD conformance & UTF-8-safe XML validation:**
   - Default export is MusicXML 3.1 (`to_musicxml`), with 4.0 opt-in (`to_musicxml_version`).
   - Emit composer metadata inside `<identification><creator type="composer">...</creator></identification>` (never top-level `<creator>`).
   - For grace notes and grace chords, emit `<grace/>` **before** `<chord/>`, with no `<duration>` element (zero-width events; layout never advances the cursor).
   - `validate_musicxml` must remain UTF-8-safe (never slice XML strings on arbitrary byte offsets mid-codepoint) while checking XML declaration, `<score-partwise>`, entity escaping, and per-measure tick sums across `<backup>`, `<forward>`, `<chord/>`, and `<grace/>`.
4. **Feature-gate discipline:**
   - `default = ["cli", "bekern", "lattice"]`, `cli = ["dep:clap", "bekern", "lattice"]`, `bekern = []`, `lattice = []`.
   - Core library (`--no-default-features`) must always compile cleanly and pass all core tests on both native targets and `wasm32-unknown-unknown` without `clap`, `bekern`, `decode`, or `lattice`.
   - Keep `[[bin]] name = "mscz"` gated behind `required-features = ["cli"]` and `04_bekern_ml_roundtrip` behind `required-features = ["bekern"]`.
5. **Explicit `LossTable` discipline:**
   - Never silently drop, truncate, or fudge musical data. Every approximation or structural normalization must be logged in `LossTable` with the proper severity:
     - `"loss"`: output no longer encodes input bit-for-bit (`pitch-microtone-rounded`, `overlap-clamped`, `note-dropped-overlap`, `note-type-approximated`).
     - `"info"`: lossless structural normalization (`rest-inserted`).
   - Maintain `LossTable::is_lossless()` / `LossTable::loss_count()` accuracy so callers can gate pipelines deterministically.

## Conventions

- **Determinism:** Identical inputs must produce byte-identical outputs. Use `BTreeMap` / `BTreeSet` (never `HashMap`) whenever iteration order can affect XML, JSON, or `.mscz` output.
- **Backwards-compatible IR extensions:** New fields on `NoteInput`, `MeasureAttr`, or `ScoreInput` must use `#[serde(default)]` (and `skip_serializing_if = "Option::is_none"` on `MeasureAttr`) so existing JSON note lists keep parsing.
- **Adding a notation feature:** (1) extend IR + `parse_score`, (2) emit in both `to_musicxml` and `to_mscx` (using `mscx_location_xml` for spanners), (3) log any approximation in `LossTable`, (4) wire `musicxml_import` / `bekern` / `decode` if in the `bekern+` vocabulary, (5) add round-trip tests asserting zero validator errors and exact `LossTable` behavior (and spot-check `.mscz` in MuseScore 4 desktop if `.mscx` output changed).
- **Docs:** Keep `README.md`, `ARCHITECTURE.md`, `CONTRIBUTING.md`, and `CHANGELOG.md` aligned with public API, CLI subcommands, and feature flags. `docs.rs` builds with `all-features = true`.

## Release Checklist (`v0.1.0` Live)

`v0.1.0` is published on GitHub (`https://github.com/mscz-rs/mscz-rs`) and `crates.io` (`mscz`). For subsequent releases (`v0.x.y`):

1. Bump `version` in `Cargo.toml` (SemVer) and move unreleased notes into a dated section in `CHANGELOG.md`.
2. Run all 7 verification commands above and confirm zero warnings/errors.
3. Verify `cargo publish --dry-run` (checks packaged file list, license files, README, and build from tarball).
4. Spot-check generated `.mscz` archives in MuseScore 4 desktop when `.mscx` emission changed (zero repair dialogs).
5. Tag `vX.Y.Z` and push to trigger automated publishing and GitHub Release creation (see below).

## CI & Release Automation

- **CI (`.github/workflows/ci.yml`):** Runs on every `push` and `pull_request` to `main` across `ubuntu-latest`, `windows-latest`, and `macos-latest` (`cargo fmt --all -- --check` on Ubuntu, `clippy` + `test` for both `--all-features` and `--no-default-features`, `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features`, and `wasm32-unknown-unknown` check on Ubuntu).
- **Release (`.github/workflows/release.yml`):** Configure the repository secret `CARGO_REGISTRY_TOKEN` (a `crates.io` API token) in GitHub Settings → Secrets and variables → Actions. To cut a release after bumping `Cargo.toml` and `CHANGELOG.md` on `main`:
  ```bash
  git tag vX.Y.Z && git push origin vX.Y.Z
  ```
  The workflow verifies `Cargo.toml` matches `vX.Y.Z`, re-runs the full clippy/test/doc suite, publishes to `crates.io`, and creates a GitHub Release with auto-generated notes. Manual `workflow_dispatch` runs default to `cargo publish --dry-run` (and skip upload gracefully if `CARGO_REGISTRY_TOKEN` is unset).
