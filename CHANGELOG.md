# Changelog

All notable changes to `mscz` (`mscz-rs`) will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2026-11-24

Initial public release of `mscz` (`mscz-rs`), a pure-Rust, zero-GPL symbolic music notation serialization, decoding, and validation library and CLI.

### Added

- **480-tick Score IR (`parse_score`) & Fluent Builders (`ScoreBuilder`, `PartBuilder`)**: Deterministic integer-tick score assembly supporting multi-part, multi-voice notation, simultaneous chord grouping, automatic barline-crossing tie splitting, voice-gap rest insertion, and mid-stream clef/key/time signature changes via `MeasureAttr`.
- **Native MuseScore 4 `.mscx` & `.mscz` writer & reader (`to_mscx`, `pack_mscz`, `pack_mscz_bytes`, `read_mscz`)**:
  - Generates MuseScore 4 (`version="4.20"`) `.mscx` with `<Part>`, `<Instrument>`, `<Staff>`, `<Clef>`, `<KeySig>` (`<concertKey>`), `<TimeSig>`, `<Chord>`/`<Note>` with exact Tonal Pitch Class (`<tpc>`), `<Rest>` (including full-measure rests), `<appoggiatura/>` grace chords, `<Dynamic>`, `<Articulation>`, `<Fermata>`, `<Lyrics>`, and `<BarLine>` / repeats.
  - Pairs cross-measure and cross-voice `Tie`, `Slur`, and `HairPin` spanners using GCD-reduced `<location>` fractions.
  - Packs and inspects in-memory deflated `.mscz` archives containing `META-INF/container.xml`, `<stem>.mscx`, and `score_style.mss`.
  - Live-verified against MuseScore 4.7.5.
- **MusicXML 3.1-first / 4.0 opt-in serializer (`to_musicxml`, `to_musicxml_version`)**: Generates `<score-partwise version="3.1">` (or `4.0`) with exact tick `<duration>`, multi-voice `<backup>` layout, ties, slurs, articulations, fermatas, dynamics, hairpins, lyrics, grace notes, and barlines.
- **Transparent `LossTable`**: Structured `loss` / `info` audit log recording microtone rounding (`pitch-microtone-rounded`), voice overlap resolution (`overlap-clamped`, `note-dropped-overlap`), display type approximation (`note-type-approximated`), and gap rest insertion (`rest-inserted`), with `LossTable::is_lossless()` and `LossTable::loss_count()`.
- **Three-stage validators**: `validate_notes`, `validate_musicxml` (full per-measure tick-sum simulation), and `validate_mscz` (ZIP + container + `.mscx` integrity checks).
- **MusicXML 3.1 importer (`musicxml_import`)**: Imports whole scores or paginated pages (`<print new-page="yes">`) into canonical `ScoreInput`, extracts engraved `system_breaks`, and splits scores into N-measure chunks with carried headers (`split_measures`).
- **Canonical `bekern+` codec (`bekern`, `decode`)**: Bidirectional MusicXML ↔ `bekern+` encoding/decoding with merged and link-faithful tie modes, authoritative `<time-modification>` tuplet scaling, grace notes, dynamics, hairpins, articulations, and barlines.
- **OMR evaluation & review helpers**: Symbol Error Rate (`ser_for_inputs`, `ser_for_symbols`, `levenshtein`) and measure-atomic top-2 candidate lattice sidecar ranking (`lattice`).
- **CLI (`mscz`)**: 12 subcommands (`example`, `serialize`, `mscz`, `pack`, `validate`, `import`, `bekern`, `bekern-decode`, `bekern2musicxml`, `split`, `system-breaks`, `ser`).
- **Dual permissive license**: `MIT OR Apache-2.0` with zero GPL dependencies.
