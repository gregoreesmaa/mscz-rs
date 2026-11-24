# `mscz` (`mscz-rs`)

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)
[![Rust 1.88+](https://img.shields.io/badge/rust-1.88%2B-orange.svg)](https://www.rust-lang.org)
[![Zero GPL](https://img.shields.io/badge/GPL%20deps-0-brightgreen.svg)](#license)
[![WASM Ready](https://img.shields.io/badge/wasm32-ready-purple.svg)](#wasm--browser-embedding--feature-flags)

**Native MuseScore 4 (`.mscz` / `.mscx`), MusicXML 3.1/4.0, and canonical `bekern+` — in pure Rust, with zero GPL dependencies.**

---

## Why `mscz` exists

Every music app that wants to export `.mscz` or clean MusicXML hits the same wall: shell out to GPL MuseScore, wrestle with heavy Python `music21`, or hand-roll fragile XML.

`mscz` (`mscz-rs`) is a tiny, zero-GPL, pure-Rust engine for native MuseScore 4 `.mscz`, MusicXML 3.1/4.0 (with an explicit loss table), and canonical `bekern+` — embeddable anywhere from CLI tools and OMR pipelines to browsers (WASM).

- **Native MuseScore 4 `.mscz` archives** (`version="4.20"`): complete `<Part>`, `<Instrument>`, `<Staff>`, `<Clef>`, `<KeySig>`, `<TimeSig>`, `<Chord>`/`<Note>` with exact Tonal Pitch Class (`<tpc>`), `<Rest>`, `<appoggiatura/>` grace notes, GCD-reduced `<location>` spanners (`Tie`, `Slur`, `HairPin`), `<Dynamic>`, `<Articulation>`, `<Fermata>`, `<Lyrics>`, and `<BarLine>` / repeats — packed with `META-INF/container.xml` and `score_style.mss`.
- **MusicXML 3.1-first (4.0 opt-in)** with measure-exact tick durations, lossless barline-crossing ties, multi-voice `<backup>` layout, and built-in validators.
- **Explicit `LossTable`**: never silently drops a microtone, clamps an overlap, or approximates a note type — every approximation is structured, countable, and inspectable.
- **Canonical `bekern+` codec**: bidirectional MusicXML ↔ `bekern+` token streams for optical music recognition (OMR) and symbolic music ML, plus Symbol Error Rate (`ser`) and measure-confidence lattice sidecars.
- **Zero-GPL & WASM-ready**: dual-licensed `MIT OR Apache-2.0`, zero C++ FFI, pure-Rust `deflate` zip, zero filesystem I/O in library mode.

---

## 15-Line Rust Quickstart

```rust
use mscz::{pack_mscz_bytes, LossTable, ScoreBuilder};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (score, mut losses): (_, LossTable) = ScoreBuilder::new("Nocturne Sketch")
        .composer("Ada Lovelace")
        .time(4, 4)
        .key_fifths(1) // G major
        .part("Piano", |p| {
            p.note("G4", 0.0, 1.0).note("B4", 1.0, 1.0)
             .note("D5", 2.0, 1.0).note("G5", 3.0, 2.0) // crosses barline -> tied!
        })
        .build()?;

    let mscz_bytes: Vec<u8> = pack_mscz_bytes(&score, &mut losses)?;
    assert!(losses.is_lossless());
    std::fs::write("nocturne.mscz", mscz_bytes)?;
    Ok(())
}
```

> **Note:** Need the raw `.mscx` or MusicXML string too? Call `to_mscx(&score, &mut losses)`, `to_musicxml(&score, &mut losses)` (3.1 default), or `to_musicxml_version(&score, version, &mut losses)` (3.1 or 4.0) on the same `Score` and `LossTable`, or `read_mscz(&mscz_bytes)` to inspect an existing `.mscz` archive in memory.

---

## CLI Quickstart

Install the `mscz` binary:

```bash
cargo install mscz
```

Generate a sample score, export `.mscz`, pack a full validated bundle, and round-trip MusicXML or `bekern+`:

```bash
# 1. Write a starter note-list JSON file
mscz example --out example-notes.json

# 2. Convert straight to a native MuseScore 4 .mscz file
mscz mscz --input example-notes.json --out example.mscz

# 3. Full bundle: .musicxml + .mscx + .mscz + loss.json + validation.json
mscz pack --input example-notes.json --out out/

# 4. Validate note-list JSON, MusicXML, and .mscz together
mscz validate --input example-notes.json --musicxml out/Two-Voice-Prelude.musicxml --mscz out/Two-Voice-Prelude.mscz --report validation.json

# 5. Import an existing MusicXML score into canonical note-list JSON (whole score or --pages)
mscz import --input tests/fixtures/ds0001.musicxml --out imported/ --pages

# 6. Encode page 1 to a bekern+ token stream, then decode straight into a full validated bundle
mscz bekern --input tests/fixtures/ds0001.musicxml --out . --page 1 --stem page
mscz bekern2musicxml --input page-bekern.txt --out out/
```

### All CLI Subcommands

| Subcommand | What it does |
|---|---|
| `mscz example` | Write a two-bar starter note-list JSON file |
| `mscz serialize` | Note-list JSON → `<stem>.musicxml` + `loss.json` |
| `mscz mscz` | Note-list JSON → native MuseScore 4 `.mscz` |
| `mscz pack` | Full pipeline: `.musicxml` + `.mscx` + `.mscz` + `loss.json` + `validation.json` (plus optional `--lattice-in`) |
| `mscz validate` | Run structural & duration-sum validators over note-list JSON, MusicXML, and `.mscz` |
| `mscz import` | MusicXML 3.1 → canonical note-list JSON (whole score or per-page with `--pages`) |
| `mscz bekern` | MusicXML 3.1 → canonical `bekern+` token stream (`--page N` or `--measures START..END`) |
| `mscz bekern-decode` | `bekern+` token stream → note-list JSON (merged tie chains) |
| `mscz bekern2musicxml` | `bekern+` token stream → full `pack` bundle (link-faithful ties, optional `--lattice-in`) |
| `mscz split` | Split a MusicXML score into consecutive N-measure chunks carrying running headers |
| `mscz system-breaks` | Extract engraved `(page, system, measure span)` breaks from MusicXML as JSON |
| `mscz ser` | Compute Symbol Error Rate (Levenshtein edit distance) between reference and hypothesis JSON |

---

## Honest Comparison

`mscz` is a **symbolic notation and serialization engine**, not a visual engraving engine. If you need in-browser sheet-music SVG rendering, pair `mscz` (generating clean MusicXML / `.mscz`) with **Verovio** for instant SVG preview.

| Capability | `mscz` (`mscz-rs`) | MuseScore CLI | `music21` (Python) | Verovio (C++/WASM) |
|---|---|---|---|---|
| **License** | **MIT OR Apache-2.0** | GPLv3 | BSD-3-Clause | LGPLv3 |
| **Native `.mscz` / `.mscx` export** | **Yes (MS4 4.20, pure Rust)** | Yes (authoritative) | No (MusicXML only) | No |
| **MusicXML 3.1 export** | **Yes + explicit `LossTable`** | Yes | Yes | Import / MEI |
| **`bekern+` OMR/ML codec** | **Yes (encode + decode + SER)** | No | Humdrum `**kern` | Humdrum `**kern` |
| **Visual engraving (SVG / PDF / PNG)** | **No — pair with Verovio** | Yes | Via MuseScore/LilyPond | **Yes (best-in-class)** |
| **Runtime footprint** | Single native binary or small WASM | ~300 MB Qt desktop app | Python + NumPy/SciPy | C++ / ~4 MB WASM |
| **Headless / server / browser safety** | **Zero display server, zero FFI** | Needs Qt/platform plugin | Server only (no browser) | Server + browser |

---

## The Transparent Loss-Table Philosophy

Most notation converters silently fudge reality: a 30-cent microtone vanishes, an overlapping note in voice 1 gets quietly truncated, or a 160-tick triplet gets drawn as an eighth note with no warning.

`mscz` takes the opposite stance: **never fail on mildly messy musical input, and never lie about what changed.** Every normalization or approximation is recorded in a structured [`LossTable`](src/lib.rs) with two severities:

- **`"loss"`** — the output no longer encodes the input bit-for-bit (`pitch-microtone-rounded`, `overlap-clamped`, `note-dropped-overlap`, `note-type-approximated`).
- **`"info"`** — lossless structural normalization worth auditing (`rest-inserted` for voice gaps).

```json
{
  "entries": [
    {
      "severity": "loss",
      "kind": "pitch-microtone-rounded",
      "message": "pitch \"A4+30c\" carries +30 cents; rounded to MIDI 69",
      "part": "Violin",
      "note_index": 0
    },
    {
      "severity": "loss",
      "kind": "overlap-clamped",
      "message": "onset moved +240 ticks to resolve voice-1 overlap",
      "part": "Violin",
      "note_index": 1
    },
    {
      "severity": "info",
      "kind": "rest-inserted",
      "message": "voice 1 measure 1: gap filled with rests",
      "part": "Violin"
    }
  ]
}
```

In Rust, gate downstream pipelines with a single check:

```rust
if !losses.is_lossless() {
    eprintln!("{} lossy approximation(s) recorded", losses.loss_count());
}
```

---

## Feature Support & Engraver Compatibility

### Notation Feature Support

| Feature | MusicXML 3.1 | Native MS4 `.mscx` / `.mscz` | `bekern+` Codec | Notes / Boundaries |
|---|---|---|---|---|
| Pitches, accidentals (`#`, `b`, `x`, `bb`), enharmonic TPC | Yes | Yes | Yes | Microtones (`+30c`) round to MIDI + log `pitch-microtone-rounded` |
| Chords & multi-voice per part | Yes (`<chord/>`, `<backup>`) | Yes (`<Chord>`, `<voice>`) | Yes | Simultaneous same-voice notes group into chords |
| Rests & full-measure rests | Yes | Yes (`measure` rest) | Yes (`bar-rest`) | Voice gaps auto-fill with rests (`info` in `LossTable`) |
| Barline-crossing ties & explicit tie links | Yes (`<tie>` + `<tied>`) | Yes (`<Spanner type="Tie">`) | Yes | Automatic lossless split at barlines |
| Grace notes | Yes (`<grace/>`) | Yes (`<appoggiatura/>`) | Yes | Zero-width events; grace rests skipped in MS4 |
| Slurs | Yes | Yes (`<Spanner type="Slur">`) | Yes | Single slur layer (`number="1"`) per voice |
| Dynamics & hairpins | Yes (`<direction>`) | Yes (`<Dynamic>`, `HairPin`) | Yes | Measure-scoped (`pp..ff`, `sfz`, `cresc`, `dim`, `stop`) |
| Articulations & fermatas | Yes | Yes | Yes | Closed set: `stacc`, `accent`, `tenuto`, `fermata` |
| Mid-stream clef, key, time changes | Yes | Yes | Yes | Clefs: G/F/C lines, 8va/8vb/15ma/15mb, percussion |
| Repeats & barlines | Yes | Yes | Yes | `repeat-left`, `repeat-right`, `double`, `light-heavy` |
| Lyrics | Yes (1 verse) | Yes (1 verse) | No | Single `<lyric>` / `<Lyrics>` line per note/chord |
| Tuplets | Exact `<duration>`, approx `<type>` | Approx `<durationType>` | Yes (`tuplet-start-A-N`) | Performed timing is exact; visual `<time-modification>` export not yet emitted (logged in `LossTable`) |
| Grand-staff piano | Two parts | Two parts | Multi-staff | Single `<Staff>` per `<Part>` today; model RH/LH as two parts |

### Engraver Compatibility Matrix

| Engraver / Tool | Import Path | Verification Status | Notes |
|---|---|---|---|
| **MuseScore 4.7.5** | Native `.mscz` / `.mscx` & MusicXML 3.1 | **Live-verified** | Opens native `.mscz` directly with zero repair dialogs; clefs, TPCs, spanners, multi-voice, dynamics, hairpins, barlines, and lyrics verified |
| **Dorico** | MusicXML 3.1 | Standard MusicXML 3.1 | Clean `<score-partwise version="3.1">` with exact measure sums and `<backup>` voices |
| **Sibelius** | MusicXML 3.1 | Standard MusicXML 3.1 | Standard partwise import; grand-staff piano imports as two independent parts |
| **Finale** | MusicXML 3.1 | Standard MusicXML 3.1 | Validated against Recordare MusicXML 3.1 partwise conventions |
| **Verovio** | MusicXML 3.1 | Standard MusicXML 3.1 | Ideal companion for zero-GPL browser SVG preview alongside `mscz` |
| **LilyPond** | MusicXML 3.1 (`musicxml2ly`) | Standard MusicXML 3.1 | Compatible via `musicxml2ly`; tuplets render at quantized display types unless `bekern+` is used |

---

## WASM / Browser Embedding & Feature Flags

`mscz` separates its core library from the CLI so you can embed the full symbolic engine in `wasm32-unknown-unknown` with **zero filesystem I/O** and **no `clap` dependency**:

```toml
[dependencies]
mscz = { version = "0.1", default-features = false }
```

| Feature | Default | What it enables |
|---|---|---|
| `cli` | Yes | Builds the `mscz` binary with `clap` argument parsing |
| `bekern` | Yes | Enables canonical `bekern+` encoding and decoding |
| `lattice` | Yes | Enables measure-atomic top-2 lattice sidecar validation and ranking |
| *(no default features)* | — | Pure-Rust library (`serde`, `serde_json`, pure-Rust `deflate` `zip`), compiles cleanly to `wasm32-unknown-unknown`, all serializers/validators/packers operate on `&str` and `Vec<u8>` in memory |

Check WASM compilation locally:

```bash
cargo check --no-default-features --target wasm32-unknown-unknown
```

---

## Runnable Examples

Explore end-to-end workflows in [`examples/`](examples/):

- [`examples/01_compose_two_voice.rs`](examples/01_compose_two_voice.rs) — Compose a two-voice miniature prelude with slurs, articulations, cross-bar ties, and an intentional microtone; export MusicXML 3.1 + native `.mscz` and inspect `LossTable`.
- [`examples/02_musicxml_to_mscz.rs`](examples/02_musicxml_to_mscz.rs) — Import an OpenScore MusicXML 3.1 file (`tests/fixtures/ds0001.musicxml` by default), pack a validated native MuseScore 4 `.mscz` archive.
- [`examples/03_validate_and_inspect.rs`](examples/03_validate_and_inspect.rs) — CI-style validation and structured `Issue` inspection across `validate_notes`, `validate_musicxml`, and `validate_mscz` on both clean and deliberately corrupted inputs.
- [`examples/04_bekern_ml_roundtrip.rs`](examples/04_bekern_ml_roundtrip.rs) — Round-trip MusicXML → `bekern+` tokens → decoded `ScoreInput` → `.mscz` and compute Symbol Error Rate (`ser_for_inputs`).

Run any example with:

```bash
cargo run --example 01_compose_two_voice
```

---

## Architecture & Contributing

- Read [`ARCHITECTURE.md`](ARCHITECTURE.md) for the dataflow diagram, 480-tick IR, Tonal Pitch Class (TPC) math, and GCD-reduced MuseScore 4 `<location>` spanners.
- Read [`CONTRIBUTING.md`](CONTRIBUTING.md) to run the test suite, clippy, and WASM checks, or add new notation features.
- Read [`CHANGELOG.md`](CHANGELOG.md) for release history.

---

## License

Dual-licensed under either:

- **MIT License** ([`LICENSE-MIT`](LICENSE-MIT))
- **Apache License, Version 2.0** ([`LICENSE-APACHE`](LICENSE-APACHE))

at your option. Zero GPL or AGPL dependencies anywhere in the dependency tree.
