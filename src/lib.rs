//! `mscz`: flat note list -> MusicXML 3.1/4.0 + loss table +
//! validators -> `.mscz` (MuseScore 4) zip, plus MusicXML import, `.mscz`
//! inspection, and `bekern+` encoding/decoding.
//!
//! Pipeline:
//! 1. [`parse_score`] turns a [`ScoreInput`] (JSON note list) into a tick-based
//!    [`Score`]. Anything that cannot be represented exactly is kept going
//!    with a nearest approximation and recorded in the [`LossTable`].
//! 2. [`to_musicxml`] (3.1 default) or [`to_musicxml_version`] (3.1 or 4.0)
//!    renders MusicXML partwise (lossless except for the entries already in the
//!    loss table; barline crossings become ties).
//! 3. [`to_mscx`] renders a MuseScore 4 native `.mscx` (`version="4.20"`)
//!    with parts, instruments, clefs, key/time signatures, multi-voice layout,
//!    chords, rests, grace notes, ties, slurs, hairpins, dynamics,
//!    articulations, fermatas, lyrics, and barlines.
//! 4. [`pack_mscz`] / [`pack_mscz_bytes`] zips `META-INF/container.xml`,
//!    `<stem>.mscx`, and `score_style.mss` into `.mscz`; [`read_mscz`] unpacks
//!    and inspects an existing `.mscz` archive.
//! 5. [`validate_notes`], [`validate_musicxml`], [`validate_mscz`] check each
//!    stage; [`Issue`] lists use `error` / `warning` severities.
//!
//! All durations are integer ticks: `ticks = round(beats * divisions)` with
//! `divisions` (default 480) quarter-note ticks per quarter note.
//!
//! # Quick start
//!
//! ```
//! use mscz::{
//!     has_errors, pack_mscz_bytes, parse_score, read_mscz, to_musicxml,
//!     validate_mscz, validate_musicxml, LossTable, NoteInput, ScoreInput,
//! };
//!
//! let input = ScoreInput {
//!     title: Some("Mini Etude".into()),
//!     composer: Some("Example".into()),
//!     notes: vec![
//!         NoteInput::note("C4", 0.0, 1.0),
//!         NoteInput::note("E4", 1.0, 1.0),
//!         NoteInput::note("G4", 2.0, 2.0),
//!     ],
//!     ..Default::default()
//! };
//! let (score, mut losses) = parse_score(&input).unwrap();
//! let xml = to_musicxml(&score, &mut losses);
//! assert!(!has_errors(&validate_musicxml(&xml)));
//!
//! let bytes = pack_mscz_bytes(&score, &mut losses).unwrap();
//! assert!(!has_errors(&validate_mscz(&bytes)));
//! let contents = read_mscz(&bytes).unwrap();
//! assert!(contents.mscx.contains("<museScore version=\"4.20\">"));
//! ```

use std::collections::BTreeMap;
use std::fmt::{self, Write as _};
use std::io::{Cursor, Read, Write};

use serde::{Deserialize, Serialize};

#[cfg(feature = "bekern")]
pub mod bekern;
pub mod builder;
#[cfg(feature = "bekern")]
pub mod decode;
#[cfg(feature = "lattice")]
pub mod lattice;
pub mod musicxml_import;
pub mod pitch;
#[cfg(feature = "wasm")]
pub mod wasm;

pub use builder::{PartBuilder, ScoreBuilder};

// ---------------------------------------------------------------------------
// Input model (JSON note list)
// ---------------------------------------------------------------------------

/// House tick grid: every [`ScoreInput`] producer (mapper, MusicXML import)
/// emits this, so SER tick symbols compare across pipelines. Matches
/// MuseScore's native divisions.
pub const SCORE_DIVISIONS: u32 = 480;

fn default_divisions() -> u32 {
    SCORE_DIVISIONS
}

fn default_voice() -> u32 {
    1
}

/// Time signature, e.g. `{"beats": 6, "beat_type": 8}` for 6/8.
/// Defaults to 4/4.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeSig {
    #[serde(default = "default_beats")]
    pub beats: u32,
    #[serde(default = "default_beat_type")]
    pub beat_type: u32,
}

impl Default for TimeSig {
    fn default() -> Self {
        Self {
            beats: default_beats(),
            beat_type: default_beat_type(),
        }
    }
}

fn default_beats() -> u32 {
    4
}
fn default_beat_type() -> u32 {
    4
}

/// One note (or rest) of the input list.
///
/// * `pitch`: scientific pitch like `"C4"`, `"F#5"`, `"Bb3"`, `"Gx2"`
///   (`x` = double sharp). A trailing `+NNc`/`-NNc` cents suffix
///   (e.g. `"A4+30c"`) is rounded to the nearest semitone and logged as loss.
/// * `rest: true` (or missing pitch with `rest: true`) marks a rest.
/// * `onset` / `dur` are in beats (quarter-note units).
/// * `voice` is 1-based; simultaneous same-voice onsets become chords.
/// * `grace`: a grace note (occupies no time; `dur` is the engraved
///   display value used for `<type>`). Renders with `<grace/>` and no
///   `<duration>`, so it scores as a zero-duration event.
/// * `slur_start` / `slur_stop`: slur spanner endpoints on this note.
/// * `articulations`: closed subset (`"stacc"`, `"accent"`, `"tenuto"`,
///   `"fermata"`), rendered into `<notations>`.
/// * `tie_start` / `tie_stop`: tie links (rendered as `<tie>`/`<tied>`;
///   link-faithful, never merged here — merging is the caller's choice).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoteInput {
    #[serde(default)]
    pub pitch: Option<String>,
    #[serde(default)]
    pub rest: bool,
    pub onset: f64,
    pub dur: f64,
    #[serde(default = "default_voice")]
    pub voice: u32,
    #[serde(default)]
    pub lyric: Option<String>,
    #[serde(default)]
    pub grace: bool,
    #[serde(default)]
    pub slur_start: bool,
    #[serde(default)]
    pub slur_stop: bool,
    #[serde(default)]
    pub articulations: Vec<String>,
    #[serde(default)]
    pub tie_start: bool,
    #[serde(default)]
    pub tie_stop: bool,
}

impl Default for NoteInput {
    fn default() -> Self {
        Self {
            pitch: None,
            rest: false,
            onset: 0.0,
            dur: 1.0,
            voice: default_voice(),
            lyric: None,
            grace: false,
            slur_start: false,
            slur_stop: false,
            articulations: Vec::new(),
            tie_start: false,
            tie_stop: false,
        }
    }
}

impl NoteInput {
    /// Construct a pitched note in voice 1 at `onset` beats with duration `dur` beats.
    pub fn note(pitch: impl Into<String>, onset: f64, dur: f64) -> Self {
        Self {
            pitch: Some(pitch.into()),
            onset,
            dur,
            ..Self::default()
        }
    }

    /// Construct an explicit rest in voice 1 at `onset` beats with duration `dur` beats.
    pub fn rest(onset: f64, dur: f64) -> Self {
        Self {
            rest: true,
            onset,
            dur,
            ..Self::default()
        }
    }

    /// Set the 1-based voice number on this note.
    pub fn with_voice(mut self, voice: u32) -> Self {
        self.voice = voice;
        self
    }
}

/// One named part (staff system) holding an unordered note list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartInput {
    #[serde(default = "default_part_name")]
    pub name: String,
    #[serde(default)]
    pub notes: Vec<NoteInput>,
}

impl Default for PartInput {
    fn default() -> Self {
        Self {
            name: default_part_name(),
            notes: Vec::new(),
        }
    }
}

fn default_part_name() -> String {
    "Part 1".to_string()
}

/// Measure-scoped notation state changes (bekern+ mid-stream headers,
/// dynamics, hairpins, barlines). `measure` is 1-based. Every field is
/// a last-wins override for that measure; absent fields inherit the
/// score state. Short spellings: `dynamic` is one of
/// `pp|p|mp|mf|f|ff|sfz`, `hairpin` one of `cresc|dim|stop`,
/// `barline_left` is `repeat-left`, `barline_right` one of
/// `light-heavy|repeat-right|double`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MeasureAttr {
    pub measure: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clef_sign: Option<char>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clef_line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clef_octave_change: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_fifths: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<TimeSig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dynamic: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hairpin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub barline_left: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub barline_right: Option<String>,
}

/// Top-level JSON document. Either `parts` or the flat `notes` shorthand
/// (which becomes one part named `part_name`) must be non-empty.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreInput {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub composer: Option<String>,
    #[serde(default = "default_divisions")]
    pub divisions: u32,
    #[serde(default)]
    pub time: Option<TimeSig>,
    /// Key signature in fifths: 0 = C major, 1 = G, -1 = F, ...
    #[serde(default)]
    pub key_fifths: i32,
    #[serde(default)]
    pub parts: Vec<PartInput>,
    #[serde(default)]
    pub notes: Vec<NoteInput>,
    #[serde(default)]
    pub part_name: Option<String>,
    /// Measure-scoped overrides (see [`MeasureAttr`]); empty when the
    /// score carries none.
    #[serde(default)]
    pub measure_attrs: Vec<MeasureAttr>,
}

impl Default for ScoreInput {
    fn default() -> Self {
        Self {
            title: None,
            composer: None,
            divisions: SCORE_DIVISIONS,
            time: None,
            key_fifths: 0,
            parts: Vec::new(),
            notes: Vec::new(),
            part_name: None,
            measure_attrs: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Internal model (integer ticks)
// ---------------------------------------------------------------------------

/// A pitch or rest with absolute tick positions.
///
/// `grace` notes are zero-width (they occupy no time): `dur_ticks`
/// carries the engraved display value for `<type>` only, and layout
/// never advances the cursor for them.
#[derive(Debug, Clone)]
pub struct ScoreNote {
    pub step: char,
    pub alter: i32,
    pub octave: i32,
    pub midi: i32,
    pub start_ticks: i64,
    pub dur_ticks: i64,
    pub voice: u32,
    pub rest: bool,
    pub lyric: Option<String>,
    pub grace: bool,
    pub slur_start: bool,
    pub slur_stop: bool,
    pub articulations: Vec<String>,
    pub tie_start: bool,
    pub tie_stop: bool,
}

/// Simultaneous same-voice notes sharing one onset/duration.
#[derive(Debug, Clone)]
pub struct ChordGroup {
    pub start_ticks: i64,
    pub dur_ticks: i64,
    pub voice: u32,
    pub members: Vec<ScoreNote>,
}

#[derive(Debug, Clone)]
pub struct Part {
    pub name: String,
    pub groups: Vec<ChordGroup>,
    pub voices: Vec<u32>,
    pub end_ticks: i64,
}

/// Fully resolved score plus the loss table accumulated while building it.
#[derive(Debug, Clone)]
pub struct Score {
    pub title: String,
    pub composer: Option<String>,
    pub divisions: u32,
    pub beats: u32,
    pub beat_type: u32,
    pub key_fifths: i32,
    pub parts: Vec<Part>,
    pub measure_attrs: Vec<MeasureAttr>,
}

impl Score {
    pub fn measure_ticks(&self) -> i64 {
        self.beats as i64 * self.divisions as i64 * 4 / self.beat_type as i64
    }

    /// Per-measure `[start_ticks, end_ticks)` windows respecting any
    /// mid-stream time-signature changes in `measure_attrs`.
    pub fn measure_windows(&self) -> Vec<(i64, i64)> {
        let end = self
            .parts
            .iter()
            .map(|p| p.end_ticks)
            .max()
            .unwrap_or(0)
            .max(1);
        let mut windows = Vec::new();
        let mut cur = 0i64;
        let mut beats = self.beats;
        let mut beat_type = self.beat_type;
        let mut m = 1u32;
        while cur < end {
            if let Some(t) = measure_attr(self, m).and_then(|a| a.time) {
                beats = t.beats;
                beat_type = t.beat_type;
            }
            let mlen = (beats as i64 * self.divisions as i64 * 4 / beat_type.max(1) as i64).max(1);
            windows.push((cur, cur + mlen));
            cur += mlen;
            m += 1;
        }
        windows
    }

    pub fn measure_count(&self) -> usize {
        self.measure_windows().len()
    }
}

// ---------------------------------------------------------------------------
// Loss table
// ---------------------------------------------------------------------------

/// One lossy (or informational) approximation. `severity` is `"loss"` when
/// the output no longer encodes the input exactly, `"info"` for lossless
/// normalisation (gap rests, sorting) worth knowing about.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LossEntry {
    pub severity: String,
    pub kind: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub part: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note_index: Option<usize>,
}

/// Ordered loss/info log for one conversion run.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LossTable {
    pub entries: Vec<LossEntry>,
}

impl LossTable {
    fn push(
        &mut self,
        severity: &str,
        kind: &str,
        message: String,
        part: Option<String>,
        note_index: Option<usize>,
    ) {
        self.entries.push(LossEntry {
            severity: severity.to_string(),
            kind: kind.to_string(),
            message,
            part,
            note_index,
        });
    }

    pub fn loss_count(&self) -> usize {
        self.entries.iter().filter(|e| e.severity == "loss").count()
    }

    pub fn is_lossless(&self) -> bool {
        self.loss_count() == 0
    }

    /// Count of entries grouped by `kind` (sorted deterministically).
    pub fn counts_by_kind(&self) -> BTreeMap<String, usize> {
        let mut counts = BTreeMap::new();
        for e in &self.entries {
            *counts.entry(e.kind.clone()).or_insert(0) += 1;
        }
        counts
    }

    /// Compact one-line summary of losses and informational entries.
    pub fn summary(&self) -> String {
        if self.entries.is_empty() {
            return "lossless (0 entries)".to_string();
        }
        let losses = self.loss_count();
        let infos = self.entries.len() - losses;
        let by_kind: Vec<String> = self
            .counts_by_kind()
            .into_iter()
            .map(|(k, n)| format!("{k}={n}"))
            .collect();
        format!("{losses} loss(es), {infos} info ({})", by_kind.join(", "))
    }
}

impl fmt::Display for LossTable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.summary())?;
        for e in &self.entries {
            write!(f, "\n  [{}] {}: {}", e.severity, e.kind, e.message)?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Validation issues
// ---------------------------------------------------------------------------

/// One validator finding; `severity` is `"error"` or `"warning"`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    pub severity: String,
    pub code: String,
    pub message: String,
}

impl fmt::Display for Issue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}: {}", self.severity, self.code, self.message)
    }
}

fn error(code: &str, message: String) -> Issue {
    Issue {
        severity: "error".to_string(),
        code: code.to_string(),
        message,
    }
}

fn warning(code: &str, message: String) -> Issue {
    Issue {
        severity: "warning".to_string(),
        code: code.to_string(),
        message,
    }
}

pub fn has_errors(issues: &[Issue]) -> bool {
    issues.iter().any(|i| i.severity == "error")
}

// ---------------------------------------------------------------------------
// Pitch parsing (delegates to `crate::pitch`)
// ---------------------------------------------------------------------------

/// Parse `"F#5"`, `"Bb3"`, `"Gx2"`, `"A4+30c"`.
/// Returns `(step, alter, octave, midi, cents_rounded: Option<i32>)`.
///
/// Only re-spells `(step, alter, octave)` when microtonal rounding shifts the
/// MIDI note number, so e.g. `"Bb4+10c"` preserves its flat spelling.
fn parse_pitch(s: &str) -> Result<(char, i32, i32, i32, Option<i32>), String> {
    let parsed = crate::pitch::parse_pitch(s)?;
    let base_midi = crate::pitch::pitch_to_midi(parsed.step, parsed.alter, parsed.octave);
    let rounded_midi = ((base_midi as f64) + parsed.cents / 100.0).round() as i32;
    let cents_rounded = if parsed.cents != 0.0 {
        Some(parsed.cents.round() as i32)
    } else {
        None
    };
    if rounded_midi != base_midi {
        let (st, al, oct) = crate::pitch::midi_to_pitch(rounded_midi as u8, false);
        Ok((st, al, oct, rounded_midi, cents_rounded))
    } else {
        Ok((
            parsed.step,
            parsed.alter,
            parsed.octave,
            rounded_midi,
            cents_rounded,
        ))
    }
}

// ---------------------------------------------------------------------------
// Duration -> MusicXML type + dots
// ---------------------------------------------------------------------------

/// Map tick duration to `(type name, dots, actual ticks)`.
/// `actual != ticks` means the caller must log `duration-quantized` loss.
///
/// A base of `divisions / 2^k` ticks is only exact when `divisions` is a
/// multiple of `2^k`; otherwise that type (and shorter ones) cannot be
/// written exactly and is skipped, so quantisation records real loss.
pub fn quantize_duration(ticks: i64, divisions: u32) -> (&'static str, u8, i64) {
    // (type name, divisor of divisions for one base unit; negative = multiply)
    const BASES: [(&str, i64); 7] = [
        ("whole", -4),
        ("half", -2),
        ("quarter", 1),
        ("eighth", 2),
        ("16th", 4),
        ("32nd", 8),
        ("64th", 16),
    ];
    let d = divisions as i64;
    let mut best: Option<(&'static str, u8, i64)> = None;
    let mut best_dist = i64::MAX;
    let mut best_dots = u8::MAX;
    for (name, unit) in BASES {
        let base = if unit < 0 {
            d * -unit
        } else {
            if d % unit != 0 {
                continue;
            }
            d / unit
        };
        if base <= 0 {
            continue;
        }
        for dots in 0u8..=3 {
            // value = base * (2 - 2^-dots)
            let num = base * ((1 << (dots + 1)) - 1);
            let den = 1 << dots;
            if num % den != 0 {
                continue;
            }
            let value = num / den;
            if value <= 0 {
                continue;
            }
            let dist = (value - ticks).abs();
            if dist < best_dist || (dist == best_dist && dots < best_dots) {
                best_dist = dist;
                best_dots = dots;
                best = Some((name, dots, value));
            }
        }
    }
    best.unwrap_or(("64th", 0, d / 16))
}

// ---------------------------------------------------------------------------
// Score assembly
// ---------------------------------------------------------------------------

/// True when the input carries no notes at all (mirrors [`parse_score`]'s
/// `"score has no notes"` gate, without string-matching it): either `parts`
/// is non-empty-but-all-empty, or the flat `notes` shorthand is empty.
/// Scoring CLIs use this to route empty references to the exact-match
/// column instead of erroring.
pub fn is_empty_score_input(input: &ScoreInput) -> bool {
    if !input.parts.is_empty() {
        input.parts.iter().all(|p| p.notes.is_empty())
    } else {
        input.notes.is_empty()
    }
}

/// Count pitched (non-rest) chord members across all parts. Empty
/// references (zero here) score through the exact-match column, never a
/// bare F1 float; a zero here with a non-empty reference stays an
/// included F1 = 0.0.
pub fn score_pitched_count(score: &Score) -> usize {
    score
        .parts
        .iter()
        .flat_map(|p| p.groups.iter())
        .flat_map(|g| g.members.iter())
        .filter(|m| !m.rest)
        .count()
}

/// Build a [`Score`] + [`LossTable`] from validated-or-raw input.
///
/// Hard errors (`Err`): non-positive divisions/time, negative onsets,
/// non-positive durations, unparseable or out-of-range pitches, empty score.
/// Approximations (`LossTable`): microtone rounding, duration quantisation,
/// overlap clamping/drops, inserted gap rests.
pub fn parse_score(input: &ScoreInput) -> Result<(Score, LossTable), String> {
    if input.divisions == 0 {
        return Err("divisions must be positive".to_string());
    }
    let beats = input.time.map(|t| t.beats).unwrap_or(4);
    let beat_type = input.time.map(|t| t.beat_type).unwrap_or(4);
    if beats == 0 || beat_type == 0 {
        return Err("time beats and beat_type must be positive".to_string());
    }
    if !matches!(beat_type, 1 | 2 | 4 | 8 | 16 | 32) {
        return Err(format!("unsupported beat_type {beat_type}"));
    }

    let raw_parts: Vec<(String, &[NoteInput])> = if !input.parts.is_empty() {
        input
            .parts
            .iter()
            .map(|p| {
                (
                    if p.name.is_empty() {
                        "Part 1".to_string()
                    } else {
                        p.name.clone()
                    },
                    p.notes.as_slice(),
                )
            })
            .collect()
    } else {
        vec![(
            input
                .part_name
                .clone()
                .unwrap_or_else(|| "Part 1".to_string()),
            input.notes.as_slice(),
        )]
    };
    if raw_parts.iter().all(|(_, n)| n.is_empty()) {
        return Err("score has no notes".to_string());
    }

    let mut losses = LossTable::default();
    let mut parts = Vec::new();
    for (pname, notes) in &raw_parts {
        if notes.is_empty() {
            continue;
        }
        // Materialise tick notes.
        let mut tick_notes: Vec<(usize, ScoreNote)> = Vec::new();
        for (i, n) in notes.iter().enumerate() {
            if !n.onset.is_finite() || !n.dur.is_finite() {
                return Err(format!("part {pname:?} note {i}: onset/dur must be finite"));
            }
            const MAX_BEATS: f64 = 1e7;
            if n.onset < 0.0 || n.onset > MAX_BEATS {
                return Err(format!(
                    "part {pname:?} note {i}: onset {} out of range (must be in 0..={MAX_BEATS})",
                    n.onset
                ));
            }
            if n.dur <= 0.0 || n.dur > MAX_BEATS {
                return Err(format!(
                    "part {pname:?} note {i}: dur {} out of range (must be in (0, {MAX_BEATS}])",
                    n.dur
                ));
            }
            if n.voice == 0 {
                return Err(format!("part {pname:?} note {i}: voice must be >= 1"));
            }
            let start = (n.onset * input.divisions as f64).round() as i64;
            let dur = (n.dur * input.divisions as f64).round() as i64;
            if dur <= 0 {
                return Err(format!(
                    "part {pname:?} note {i}: duration {} beats rounds to zero ticks at divisions {}",
                    n.dur, input.divisions
                ));
            }
            let is_rest = n.rest
                || n.pitch
                    .as_ref()
                    .map(|p| p.trim().is_empty())
                    .unwrap_or(true);
            if is_rest {
                tick_notes.push((
                    i,
                    ScoreNote {
                        step: 'C',
                        alter: 0,
                        octave: 4,
                        midi: 60,
                        start_ticks: start,
                        dur_ticks: dur,
                        voice: n.voice,
                        rest: true,
                        lyric: n.lyric.clone(),
                        grace: n.grace,
                        slur_start: n.slur_start,
                        slur_stop: n.slur_stop,
                        articulations: n.articulations.clone(),
                        tie_start: n.tie_start,
                        tie_stop: n.tie_stop,
                    },
                ));
                continue;
            }
            let pitch = n.pitch.clone().unwrap();
            let (step, alter, octave, midi, cents) =
                parse_pitch(&pitch).map_err(|e| format!("part {pname:?} note {i}: {e}"))?;
            if let Some(c) = cents {
                losses.push(
                    "loss",
                    "pitch-microtone-rounded",
                    format!("pitch {pitch:?} carries {c:+} cents; rounded to MIDI {midi}"),
                    Some(pname.clone()),
                    Some(i),
                );
            }
            // Note: `dur` stays exact in ticks. Only the displayed
            // MusicXML <type> is approximated at render time (logged there);
            // <duration> always carries the exact tick count, so measure
            // sums hold exactly.
            tick_notes.push((
                i,
                ScoreNote {
                    step,
                    alter,
                    octave,
                    midi,
                    start_ticks: start,
                    dur_ticks: dur,
                    voice: n.voice,
                    rest: false,
                    lyric: n.lyric.clone(),
                    grace: n.grace,
                    slur_start: n.slur_start,
                    slur_stop: n.slur_stop,
                    articulations: n.articulations.clone(),
                    tie_start: n.tie_start,
                    tie_stop: n.tie_stop,
                },
            ));
        }

        // Sort by (start, voice, grace-first, midi); input order need
        // not be sorted. Grace leads its onset so the scorer reads it
        // at the pre-note cursor, matching document order on the way
        // in (a grace sorted after its note would read a full note
        // later). `sort_by_key` is stable, so remaining ties keep
        // input order.
        tick_notes.sort_by_key(|(_, n)| (n.start_ticks, n.voice, !n.grace, n.midi));

        // Group simultaneous same-voice notes into chords; clamp overlaps.
        // Grace notes are zero-width: they never clamp, drop, or advance
        // the voice cursor, and chord-merge only with matching grace-ness
        // (a real note sharing a grace onset is sequential, not a chord).
        let mut groups: Vec<ChordGroup> = Vec::new();
        let mut cursor: BTreeMap<u32, i64> = BTreeMap::new();
        for (idx, mut n) in tick_notes {
            if n.grace {
                let merge = match groups.last() {
                    Some(g) => {
                        !g.members.is_empty()
                            && g.members[0].grace
                            && g.voice == n.voice
                            && g.start_ticks == n.start_ticks
                            && g.members[0].dur_ticks == n.dur_ticks
                    }
                    None => false,
                };
                if merge {
                    groups.last_mut().unwrap().members.push(n);
                } else {
                    groups.push(ChordGroup {
                        start_ticks: n.start_ticks,
                        dur_ticks: 0,
                        voice: n.voice,
                        members: vec![n],
                    });
                }
                continue;
            }
            // Chord check first: a note starting exactly with the previous
            // group in the same voice extends the chord, it is not overlap.
            if !n.rest {
                if let Some(g) = groups.last() {
                    if !g.members[0].rest
                        && !g.members[0].grace
                        && g.voice == n.voice
                        && g.start_ticks == n.start_ticks
                        && g.dur_ticks == n.dur_ticks
                    {
                        groups.last_mut().unwrap().members.push(n.clone());
                        continue;
                    }
                }
            }
            let cur = *cursor.get(&n.voice).unwrap_or(&0);
            if n.start_ticks < cur {
                let shift = cur - n.start_ticks;
                if n.dur_ticks <= shift {
                    losses.push(
                        "loss",
                        "note-dropped-overlap",
                        format!(
                            "note fully covered by an earlier voice-{} note; dropped",
                            n.voice
                        ),
                        Some(pname.clone()),
                        Some(idx),
                    );
                    continue;
                }
                losses.push(
                    "loss",
                    "overlap-clamped",
                    format!(
                        "onset moved +{shift} ticks to resolve voice-{} overlap",
                        n.voice
                    ),
                    Some(pname.clone()),
                    Some(idx),
                );
                n.start_ticks = cur;
                n.dur_ticks -= shift;
            }
            let end = n.start_ticks + n.dur_ticks;
            cursor.insert(n.voice, end);
            groups.push(ChordGroup {
                start_ticks: n.start_ticks,
                dur_ticks: n.dur_ticks,
                voice: n.voice,
                members: vec![n],
            });
        }

        let end_ticks = groups
            .iter()
            .map(|g| g.start_ticks + g.dur_ticks)
            .max()
            .unwrap_or(0);
        let mut voices: Vec<u32> = groups.iter().map(|g| g.voice).collect();
        voices.sort();
        voices.dedup();
        parts.push(Part {
            name: pname.clone(),
            groups,
            voices,
            end_ticks,
        });
    }

    Ok((
        Score {
            title: input
                .title
                .clone()
                .unwrap_or_else(|| "Untitled".to_string()),
            composer: input.composer.clone(),
            divisions: input.divisions,
            beats,
            beat_type,
            key_fifths: input.key_fifths,
            parts,
            measure_attrs: input.measure_attrs.clone(),
        },
        losses,
    ))
}

// ---------------------------------------------------------------------------
// MusicXML 3.1 rendering
// ---------------------------------------------------------------------------

/// XML-escape text content.
pub fn escape_xml(s: &str) -> String {
    if !s
        .bytes()
        .any(|b| matches!(b, b'&' | b'<' | b'>' | b'"' | b'\''))
    {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 8);
    write_escaped_xml(&mut out, s);
    out
}

fn write_escaped_xml(out: &mut String, s: &str) {
    if !s
        .bytes()
        .any(|b| matches!(b, b'&' | b'<' | b'>' | b'"' | b'\''))
    {
        out.push_str(s);
        return;
    }
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
}

fn accidental_name(alter: i32) -> Option<&'static str> {
    match alter {
        1 => Some("sharp"),
        2 => Some("double-sharp"),
        -1 => Some("flat"),
        -2 => Some("flat-flat"),
        0 => None,
        _ => None,
    }
}

/// One rendered note/rest segment inside a measure.
struct Segment {
    note: ScoreNote,
    chord: bool, // <chord/> marker (non-first chord member)
    dur_ticks: i64,
    tie_in: bool,  // continues a previous segment -> <tie type="stop"/>
    tie_out: bool, // continues into the next segment -> <tie type="start"/>
}

/// Split a part's chord groups into per-measure, per-voice segments.
/// Groups crossing a barline are split with ties (lossless). Gaps are
/// filled with rests; every voice sums to exactly `measure_ticks`.
fn layout_part(
    score: &Score,
    part: &Part,
    losses: &mut LossTable,
) -> Vec<Vec<(u32, Vec<Segment>)>> {
    let windows = score.measure_windows();
    // voice -> sorted (start, end, member_idx, group_idx)
    let mut result: Vec<Vec<(u32, Vec<Segment>)>> = Vec::new();
    for (m, &(m0, m1)) in windows.iter().enumerate() {
        let mut voices: Vec<(u32, Vec<Segment>)> = Vec::new();
        for v in &part.voices {
            let mut segs: Vec<Segment> = Vec::new();
            for g in part.groups.iter().filter(|g| g.voice == *v) {
                // Grace groups are zero-width: they render in the one
                // bar window containing their onset (a grace exactly on
                // a barline belongs to the following measure) and never
                // split.
                let is_grace = g.members.first().is_some_and(|m| m.grace);
                if is_grace && (g.start_ticks < m0 || g.start_ticks >= m1) {
                    continue;
                }
                let gs = g.start_ticks.max(m0);
                let ge = if is_grace {
                    gs
                } else {
                    (g.start_ticks + g.dur_ticks).min(m1)
                };
                if ge <= gs && !is_grace {
                    continue;
                }
                let total = g.start_ticks + g.dur_ticks;
                for (mi, member) in g.members.iter().enumerate() {
                    segs.push(Segment {
                        note: member.clone(),
                        chord: mi > 0,
                        dur_ticks: ge - gs,
                        tie_in: gs > g.start_ticks,
                        tie_out: ge < total,
                    });
                }
            }
            segs.sort_by_key(|s| (s.note.start_ticks.max(m0), s.chord));
            // Fill gaps with rests so the voice sums to mlen.
            let mut filled: Vec<Segment> = Vec::new();
            let mut cursor = m0;
            let mut rest_inserted = false;
            for s in segs {
                let ss = s.note.start_ticks.max(m0);
                if ss > cursor {
                    filled.push(Segment {
                        note: ScoreNote {
                            step: 'C',
                            alter: 0,
                            octave: 4,
                            midi: 60,
                            start_ticks: cursor,
                            dur_ticks: ss - cursor,
                            voice: *v,
                            rest: true,
                            lyric: None,
                            grace: false,
                            slur_start: false,
                            slur_stop: false,
                            articulations: Vec::new(),
                            tie_start: false,
                            tie_stop: false,
                        },
                        chord: false,
                        dur_ticks: ss - cursor,
                        tie_in: false,
                        tie_out: false,
                    });
                    rest_inserted = true;
                }
                cursor = cursor.max(ss + s.dur_ticks);
                filled.push(s);
            }
            if cursor < m1 {
                filled.push(Segment {
                    note: ScoreNote {
                        step: 'C',
                        alter: 0,
                        octave: 4,
                        midi: 60,
                        start_ticks: cursor,
                        dur_ticks: m1 - cursor,
                        voice: *v,
                        rest: true,
                        lyric: None,
                        grace: false,
                        slur_start: false,
                        slur_stop: false,
                        articulations: Vec::new(),
                        tie_start: false,
                        tie_stop: false,
                    },
                    chord: false,
                    dur_ticks: m1 - cursor,
                    tie_in: false,
                    tie_out: false,
                });
                rest_inserted = true;
            }
            if rest_inserted {
                losses.push(
                    "info",
                    "rest-inserted",
                    format!("voice {v} measure {}: gap filled with rests", m + 1),
                    Some(part.name.clone()),
                    None,
                );
            }
            voices.push((*v, filled));
        }
        result.push(voices);
    }
    result
}

/// `<tie>` elements for one segment: barline-split articulation wins;
/// note-level tie flags render only when the segment is unsplit (a
/// split link already carries its own ties; doubling them would fake
/// extra segments to the scorer).
fn tie_elements(seg: &Segment) -> (bool, bool) {
    if seg.tie_in || seg.tie_out {
        (seg.tie_out, seg.tie_in)
    } else {
        (seg.note.tie_start, seg.note.tie_stop)
    }
}

/// Write `<notations>` block for one segment directly into `out`: tied +
/// slurs + articulations + fermata. Emits nothing when the segment carries
/// none. Rests only ever carry fermatas (the corpus has no slurred/articulated
/// rests), but the rendering is uniform.
fn write_notations_xml(out: &mut String, seg: &Segment) {
    let (tie_start, tie_stop) = tie_elements(seg);
    let has_art = seg
        .note
        .articulations
        .iter()
        .any(|a| matches!(a.as_str(), "stacc" | "accent" | "tenuto"));
    let fermata = seg.note.articulations.iter().any(|a| a == "fermata");
    if !tie_start
        && !tie_stop
        && !seg.note.slur_start
        && !seg.note.slur_stop
        && !has_art
        && !fermata
    {
        return;
    }
    out.push_str("        <notations>");
    if tie_start {
        out.push_str("<tied type=\"start\"/>");
    }
    if tie_stop {
        out.push_str("<tied type=\"stop\"/>");
    }
    if seg.note.slur_start {
        out.push_str("<slur type=\"start\" number=\"1\"/>");
    }
    if seg.note.slur_stop {
        out.push_str("<slur type=\"stop\" number=\"1\"/>");
    }
    if has_art {
        out.push_str("<articulations>");
        for a in &seg.note.articulations {
            match a.as_str() {
                "stacc" => out.push_str("<staccato/>"),
                "accent" => out.push_str("<accent/>"),
                "tenuto" => out.push_str("<tenuto/>"),
                _ => {}
            }
        }
        out.push_str("</articulations>");
    }
    if fermata {
        out.push_str("<fermata/>");
    }
    out.push_str("</notations>\n");
}

fn push_note_xml(
    out: &mut String,
    seg: &Segment,
    divisions: u32,
    losses: &mut LossTable,
    part_name: &str,
) {
    // Grace notes occupy no time: `<grace/>` plus the engraved `<type>`
    // for display, and no `<duration>` (which is also what the scorer
    // reads, so grace round-trips as zero-duration events).
    if seg.note.grace {
        out.push_str("      <note>\n");
        out.push_str("        <grace/>\n");
        if seg.chord {
            out.push_str("        <chord/>\n");
        }
        if seg.note.rest {
            out.push_str("        <rest/>\n");
        } else {
            let _ = write!(out, "        <pitch><step>{}</step>", seg.note.step);
            if seg.note.alter != 0 {
                let _ = write!(out, "<alter>{}</alter>", seg.note.alter);
            }
            let _ = writeln!(out, "<octave>{}</octave></pitch>", seg.note.octave);
        }
        let (typename, dots, shown) = quantize_duration(seg.note.dur_ticks, divisions);
        if shown != seg.note.dur_ticks {
            losses.push(
                "loss",
                "note-type-approximated",
                format!(
                    "{} ticks shown as {typename}{} ({} ticks)",
                    seg.note.dur_ticks,
                    ".".repeat(dots as usize),
                    shown
                ),
                Some(part_name.to_string()),
                None,
            );
        }
        let _ = writeln!(out, "        <voice>{}</voice>", seg.note.voice);
        let _ = writeln!(out, "        <type>{typename}</type>");
        for _ in 0..dots {
            out.push_str("        <dot/>\n");
        }
        if !seg.note.rest {
            if let Some(acc) = accidental_name(seg.note.alter) {
                let _ = writeln!(out, "        <accidental>{acc}</accidental>");
            }
        }
        write_notations_xml(out, seg);
        out.push_str("      </note>\n");
        return;
    }
    // `<duration>` carries the exact tick count (timing is lossless).
    // `<type>` is display-only: the nearest type+dots value is shown and
    // any discrepancy is logged as display loss.
    let (typename, dots, shown) = quantize_duration(seg.dur_ticks, divisions);
    if shown != seg.dur_ticks {
        losses.push(
            "loss",
            "note-type-approximated",
            format!(
                "{} ticks shown as {typename}{} ({} ticks)",
                seg.dur_ticks,
                ".".repeat(dots as usize),
                shown
            ),
            Some(part_name.to_string()),
            None,
        );
    }
    let dur = seg.dur_ticks;
    out.push_str("      <note>\n");
    if seg.chord {
        out.push_str("        <chord/>\n");
    }
    if seg.note.rest {
        out.push_str("        <rest/>\n");
    } else {
        let _ = write!(out, "        <pitch><step>{}</step>", seg.note.step);
        if seg.note.alter != 0 {
            let _ = write!(out, "<alter>{}</alter>", seg.note.alter);
        }
        let _ = writeln!(out, "<octave>{}</octave></pitch>", seg.note.octave);
    }
    let _ = writeln!(out, "        <duration>{dur}</duration>");
    let (tie_start, tie_stop) = tie_elements(seg);
    if tie_start {
        out.push_str("        <tie type=\"start\"/>\n");
    }
    if tie_stop {
        out.push_str("        <tie type=\"stop\"/>\n");
    }
    let _ = writeln!(out, "        <voice>{}</voice>", seg.note.voice);
    let _ = writeln!(out, "        <type>{typename}</type>");
    for _ in 0..dots {
        out.push_str("        <dot/>\n");
    }
    if !seg.note.rest {
        if let Some(acc) = accidental_name(seg.note.alter) {
            let _ = writeln!(out, "        <accidental>{acc}</accidental>");
        }
        write_notations_xml(out, seg);
        if let Some(ly) = &seg.note.lyric {
            out.push_str("        <lyric><text>");
            write_escaped_xml(out, ly);
            out.push_str("</text></lyric>\n");
        }
    } else {
        write_notations_xml(out, seg);
    }
    out.push_str("      </note>\n");
}

/// Write `<clef>` element for a recorded sign/line directly into `out`.
/// The token spelling for a percussion clef is `p` (single char); MusicXML
/// spells it `percussion`, and the pair re-parses to the same token.
fn write_clef_xml(out: &mut String, sign: char, line: u32, octave_change: Option<i32>) {
    out.push_str("        <clef><sign>");
    if sign == 'p' {
        out.push_str("percussion");
    } else {
        out.push(sign);
    }
    let _ = write!(out, "</sign><line>{line}</line>");
    if let Some(oct) = octave_change {
        if oct != 0 {
            let _ = write!(out, "<clef-octave-change>{oct}</clef-octave-change>");
        }
    }
    out.push_str("</clef>\n");
}

/// Measure-scoped override for one 1-based measure number, if any (last-wins).
fn measure_attr(score: &Score, measure: u32) -> Option<&MeasureAttr> {
    score.measure_attrs.iter().rfind(|a| a.measure == measure)
}

/// Write `<direction>` carrying one measure-level dynamic mark.
fn write_dyn_direction_xml(out: &mut String, dynamic: &str) {
    let _ = writeln!(
        out,
        "      <direction placement=\"below\"><direction-type><dynamics><{dynamic}/></dynamics></direction-type></direction>"
    );
}

/// Write `<direction>` carrying one hairpin wedge event.
fn write_hairpin_direction_xml(out: &mut String, hairpin: &str) {
    let wedge = match hairpin {
        "cresc" => "crescendo",
        "dim" => "diminuendo",
        _ => "stop",
    };
    let _ = writeln!(
        out,
        "      <direction placement=\"below\"><direction-type><wedge type=\"{wedge}\"/></direction-type></direction>"
    );
}

/// Write `<barline>` for a recorded bar style. The rendering re-parses to the
/// same token (repeat direction wins over style, like the importer), so
/// barlines are idempotent across round trips.
fn write_barline_xml(out: &mut String, right: bool, style: &str) {
    let loc = if right { "right" } else { "left" };
    match style {
        "repeat-left" => {
            let _ = writeln!(
                out,
                "      <barline location=\"{loc}\"><bar-style>heavy-light</bar-style><repeat direction=\"forward\"/></barline>"
            );
        }
        "repeat-right" => {
            let _ = writeln!(
                out,
                "      <barline location=\"{loc}\"><bar-style>light-heavy</bar-style><repeat direction=\"backward\"/></barline>"
            );
        }
        "light-heavy" => {
            let _ = writeln!(
                out,
                "      <barline location=\"{loc}\"><bar-style>light-heavy</bar-style></barline>"
            );
        }
        "double" => {
            let _ = writeln!(
                out,
                "      <barline location=\"{loc}\"><bar-style>light-light</bar-style></barline>"
            );
        }
        _ => {}
    }
}

/// MusicXML schema version to emit.
///
/// [`MusicXmlVersion::V31`] is the default for maximum interoperability across
/// engraving tools; [`MusicXmlVersion::V40`] is opt-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MusicXmlVersion {
    #[default]
    V31,
    V40,
}

/// Render the score as MusicXML 3.1 partwise (delegates to
/// [`to_musicxml_version`] with [`MusicXmlVersion::V31`]).
///
/// The output starts with the XML declaration and
/// `<score-partwise version="3.1">`. One `<part>` per input part, one
/// `<measure>` per bar, first voice written straight, further voices after
/// `<backup>`, each voice rest-filled to the full bar. Measure-scoped
/// overrides ([`MeasureAttr`]: mid-stream clef/key/time changes,
/// dynamics, hairpins, barlines) render as `<attributes>`,
/// `<direction>` and `<barline>` elements at their measure.
pub fn to_musicxml(score: &Score, losses: &mut LossTable) -> String {
    to_musicxml_version(score, MusicXmlVersion::V31, losses)
}

/// Render the score as MusicXML partwise at the requested [`MusicXmlVersion`]
/// (`3.1` or `4.0`).
pub fn to_musicxml_version(
    score: &Score,
    version: MusicXmlVersion,
    losses: &mut LossTable,
) -> String {
    let ver_str = match version {
        MusicXmlVersion::V31 => "3.1",
        MusicXmlVersion::V40 => "4.0",
    };
    let note_count: usize = score
        .parts
        .iter()
        .map(|p| p.groups.iter().map(|g| g.members.len()).sum::<usize>())
        .sum();
    let mut out = String::with_capacity(note_count * 220 + 1024);
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    let _ = writeln!(
        out,
        "<!DOCTYPE score-partwise PUBLIC \"-//Recordare//DTD MusicXML {ver_str} Partwise//EN\" \"http://www.musicxml.org/dtds/partwise.dtd\">"
    );
    let _ = writeln!(out, "<score-partwise version=\"{ver_str}\">");
    out.push_str("  <work><work-title>");
    write_escaped_xml(&mut out, &score.title);
    out.push_str("</work-title></work>\n");
    if let Some(c) = &score.composer {
        out.push_str("  <identification>\n    <creator type=\"composer\">");
        write_escaped_xml(&mut out, c);
        out.push_str("</creator>\n  </identification>\n");
    }
    out.push_str("  <part-list>\n");
    for (i, part) in score.parts.iter().enumerate() {
        let _ = write!(out, "    <score-part id=\"P{}\"><part-name>", i + 1);
        write_escaped_xml(&mut out, &part.name);
        out.push_str("</part-name></score-part>\n");
    }
    out.push_str("  </part-list>\n");

    let windows = score.measure_windows();
    for (i, part) in score.parts.iter().enumerate() {
        let _ = writeln!(out, "  <part id=\"P{}\">", i + 1);
        let layout = layout_part(score, part, losses);
        for (m, voices) in layout.iter().enumerate() {
            let mlen = windows
                .get(m)
                .map(|&(a, b)| b - a)
                .unwrap_or_else(|| score.measure_ticks());
            let _ = writeln!(out, "    <measure number=\"{}\">", m + 1);
            let attrs = measure_attr(score, m as u32 + 1);
            if m == 0 {
                let (clef_s, clef_l, clef_oct) = match attrs {
                    Some(a) => (
                        a.clef_sign.unwrap_or('G'),
                        a.clef_line.unwrap_or(2),
                        a.clef_octave_change,
                    ),
                    None => ('G', 2, None),
                };
                let key = attrs.and_then(|a| a.key_fifths).unwrap_or(score.key_fifths);
                let (beats, beat_type) = match attrs.and_then(|a| a.time) {
                    Some(t) => (t.beats, t.beat_type),
                    None => (score.beats, score.beat_type),
                };
                out.push_str("      <attributes>\n");
                let _ = writeln!(out, "        <divisions>{}</divisions>", score.divisions);
                let _ = writeln!(out, "        <key><fifths>{key}</fifths></key>");
                let _ = writeln!(
                    out,
                    "        <time><beats>{beats}</beats><beat-type>{beat_type}</beat-type></time>"
                );
                write_clef_xml(&mut out, clef_s, clef_l, clef_oct);
                out.push_str("      </attributes>\n");
            } else if let Some(a) = attrs {
                let has_clef = a.clef_sign.is_some()
                    || a.clef_line.is_some()
                    || a.clef_octave_change.is_some();
                if has_clef || a.key_fifths.is_some() || a.time.is_some() {
                    out.push_str("      <attributes>\n");
                    if has_clef {
                        write_clef_xml(
                            &mut out,
                            a.clef_sign.unwrap_or('G'),
                            a.clef_line.unwrap_or(2),
                            a.clef_octave_change,
                        );
                    }
                    if let Some(k) = a.key_fifths {
                        let _ = writeln!(out, "        <key><fifths>{k}</fifths></key>");
                    }
                    if let Some(t) = a.time {
                        let _ = writeln!(
                            out,
                            "        <time><beats>{}</beats><beat-type>{}</beat-type></time>",
                            t.beats, t.beat_type
                        );
                    }
                    out.push_str("      </attributes>\n");
                }
            }
            if let Some(b) = attrs.and_then(|a| a.barline_left.as_deref()) {
                write_barline_xml(&mut out, false, b);
            }
            if let Some(d) = attrs.and_then(|a| a.dynamic.as_deref()) {
                write_dyn_direction_xml(&mut out, d);
            }
            if let Some(h) = attrs.and_then(|a| a.hairpin.as_deref()) {
                write_hairpin_direction_xml(&mut out, h);
            }
            for (vi, (_voice, segs)) in voices.iter().enumerate() {
                if vi > 0 {
                    let _ = writeln!(out, "      <backup><duration>{mlen}</duration></backup>");
                }
                for s in segs {
                    push_note_xml(&mut out, s, score.divisions, losses, &part.name);
                }
            }
            if let Some(b) = attrs.and_then(|a| a.barline_right.as_deref()) {
                write_barline_xml(&mut out, true, b);
            }
            out.push_str("    </measure>\n");
        }
        out.push_str("  </part>\n");
    }
    out.push_str("</score-partwise>\n");
    out
}

// ---------------------------------------------------------------------------
// MuseScore 4 (.mscx) rendering + .mscz packing
// ---------------------------------------------------------------------------

/// Tonal pitch class for MuseScore: C=14 in the line of fifths.
fn tpc(step: char, alter: i32) -> i32 {
    crate::pitch::tonal_pitch_class(step, alter)
}

fn mscx_duration_type(dur_ticks: i64, divisions: u32) -> (&'static str, u8) {
    let (name, dots, _) = quantize_duration(dur_ticks, divisions);
    (name, dots)
}

fn gcd_i64(mut a: i64, mut b: i64) -> i64 {
    a = a.abs();
    b = b.abs();
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a.max(1)
}

/// Write a MuseScore 4 `<location>` element from a measure delta and a
/// tick offset delta directly into `out` (fractions are relative to one whole
/// note = 4 * divisions).
fn write_mscx_location_xml(out: &mut String, indent: &str, dm: i64, df_ticks: i64, divisions: u32) {
    let _ = writeln!(out, "{indent}<location>");
    if dm != 0 {
        let _ = writeln!(out, "{indent}  <measures>{dm}</measures>");
    }
    if df_ticks != 0 {
        let whole = (divisions as i64 * 4).max(1);
        let g = gcd_i64(df_ticks, whole);
        let num = df_ticks / g;
        let den = whole / g;
        let _ = writeln!(out, "{indent}  <fractions>{num}/{den}</fractions>");
    }
    let _ = writeln!(out, "{indent}  </location>");
}

/// Map MusicXML-style `(sign, line, octave_change)` to a MuseScore 4 clef type.
fn mscx_clef_type(sign: char, line: u32, octave_change: Option<i32>) -> &'static str {
    match (sign, line, octave_change.unwrap_or(0)) {
        ('G', 2, -1) => "G8vb",
        ('G', 2, 1) => "G8va",
        ('G', 2, -2) => "G15mb",
        ('G', 2, 2) => "G15ma",
        ('G', 1, _) => "G1",
        ('G', _, _) => "G",
        ('F', 4, -1) => "F8vb",
        ('F', 4, 1) => "F8va",
        ('F', 3, _) => "F3",
        ('F', 5, _) => "F5",
        ('F', _, _) => "F",
        ('C', 1, _) => "C1",
        ('C', 2, _) => "C2",
        ('C', 3, _) => "C3",
        ('C', 4, _) => "C4",
        ('C', 5, _) => "C5",
        ('C', _, _) => "C3",
        ('p' | 'P', _, _) => "PERC",
        _ => "G",
    }
}

/// One grouped voice item (a rest or a chord of 1+ simultaneous notes) in a
/// measure, with its tick offset from the start of that measure.
struct MscxItem<'a> {
    off_ticks: i64,
    dur_ticks: i64,
    is_rest: bool,
    grace: bool,
    members: Vec<&'a Segment>,
}

fn group_voice_items(segs: &[Segment]) -> Vec<MscxItem<'_>> {
    let mut items = Vec::new();
    let mut off = 0i64;
    let mut k = 0;
    while k < segs.len() {
        let head = &segs[k];
        if head.note.rest {
            let dur = if head.note.grace { 0 } else { head.dur_ticks };
            items.push(MscxItem {
                off_ticks: off,
                dur_ticks: if head.note.grace {
                    head.note.dur_ticks
                } else {
                    head.dur_ticks
                },
                is_rest: true,
                grace: head.note.grace,
                members: vec![head],
            });
            off += dur;
            k += 1;
            continue;
        }
        let mut run: Vec<&Segment> = vec![head];
        while k + run.len() < segs.len() && segs[k + run.len()].chord {
            run.push(&segs[k + run.len()]);
        }
        let is_grace = run.iter().any(|s| s.note.grace);
        let dur_display = if is_grace {
            head.note.dur_ticks
        } else {
            head.dur_ticks
        };
        let advance = if is_grace { 0 } else { head.dur_ticks };
        items.push(MscxItem {
            off_ticks: off,
            dur_ticks: dur_display,
            is_rest: false,
            grace: is_grace,
            members: run,
        });
        off += advance;
        k += items.last().unwrap().members.len();
    }
    items
}

/// Render a MuseScore 4 native `.mscx` document (`version="4.20"`).
///
/// One `<Part id="N">` + `<Staff id="N">` per part, one `<Measure>` per bar,
/// one `<voice>` per voice. Emits `<Instrument>`, `<Clef>`, `<KeySig>`
/// (`<concertKey>`), `<TimeSig>`, `<Chord>`/`<Note>`/`<pitch>`/`<tpc>`,
/// `<Rest>`, `<appoggiatura/>` grace chords, `<Spanner type="Tie">`,
/// `<Spanner type="Slur">`, `<Spanner type="HairPin">`, `<Dynamic>`,
/// `<Articulation>`, `<Fermata>`, `<Lyrics>`, and `<BarLine>` /
/// `<startRepeat/>` / `<endRepeat>`.
pub fn to_mscx(score: &Score, losses: &mut LossTable) -> String {
    let note_count: usize = score
        .parts
        .iter()
        .map(|p| p.groups.iter().map(|g| g.members.len()).sum::<usize>())
        .sum();
    let mut out = String::with_capacity(note_count * 220 + 2048);
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str("<museScore version=\"4.20\">\n");
    out.push_str("  <programVersion>4.2.0</programVersion>\n");
    out.push_str("  <Score>\n");
    let _ = writeln!(out, "    <Division>{}</Division>", score.divisions);
    out.push_str("    <showInvisible>1</showInvisible>\n");
    out.push_str("    <showUnprintable>1</showUnprintable>\n");
    out.push_str("    <showFrames>1</showFrames>\n");
    out.push_str("    <showMargins>0</showMargins>\n");
    out.push_str("    <metaTag name=\"workTitle\">");
    write_escaped_xml(&mut out, &score.title);
    out.push_str("</metaTag>\n");
    if let Some(c) = &score.composer {
        out.push_str("    <metaTag name=\"composer\">");
        write_escaped_xml(&mut out, c);
        out.push_str("</metaTag>\n");
    }
    for (i, part) in score.parts.iter().enumerate() {
        let _ = writeln!(out, "    <Part id=\"{}\">", i + 1);
        out.push_str("      <Staff>\n");
        out.push_str("        <StaffType group=\"pitched\">\n");
        out.push_str("          <name>stdNormal</name>\n");
        out.push_str("          </StaffType>\n");
        out.push_str("        </Staff>\n");
        out.push_str("      <trackName>");
        write_escaped_xml(&mut out, &part.name);
        out.push_str("</trackName>\n");
        out.push_str("      <Instrument>\n");
        out.push_str("        <longName>");
        write_escaped_xml(&mut out, &part.name);
        out.push_str("</longName>\n");
        out.push_str("        <trackName>");
        write_escaped_xml(&mut out, &part.name);
        out.push_str("</trackName>\n");
        out.push_str("        <Channel>\n");
        out.push_str("          <program value=\"0\"/>\n");
        out.push_str("          <synti>Fluid</synti>\n");
        out.push_str("          </Channel>\n");
        out.push_str("        </Instrument>\n");
        out.push_str("      </Part>\n");
    }

    let windows = score.measure_windows();

    for (i, part) in score.parts.iter().enumerate() {
        let _ = writeln!(out, "    <Staff id=\"{}\">", i + 1);
        if i == 0 {
            out.push_str("      <VBox>\n");
            out.push_str("        <height>10</height>\n");
            out.push_str("        <Text>\n");
            out.push_str("          <style>title</style>\n");
            out.push_str("          <text>");
            write_escaped_xml(&mut out, &score.title);
            out.push_str("</text>\n");
            out.push_str("          </Text>\n");
            if let Some(c) = &score.composer {
                out.push_str("        <Text>\n");
                out.push_str("          <style>composer</style>\n");
                out.push_str("          <text>");
                write_escaped_xml(&mut out, c);
                out.push_str("</text>\n");
                out.push_str("          </Text>\n");
            }
            out.push_str("        </VBox>\n");
        }

        let layout = layout_part(score, part, losses);
        let n_measures = layout.len();

        // Group each (measure, voice) into MscxItems for spanner pairing.
        let grouped: Vec<Vec<(u32, Vec<MscxItem<'_>>)>> = layout
            .iter()
            .map(|voices| {
                voices
                    .iter()
                    .map(|(v, segs)| (*v, group_voice_items(segs)))
                    .collect()
            })
            .collect();

        // Pair ties per (voice_id, midi) and slurs per voice_id across measures.
        // Keys: (measure_idx, voice_idx, item_idx, note_idx) -> (dm, df_ticks)
        let mut tie_next: BTreeMap<(usize, usize, usize, usize), (i64, i64)> = BTreeMap::new();
        let mut tie_prev: BTreeMap<(usize, usize, usize, usize), (i64, i64)> = BTreeMap::new();
        // Keys: (measure_idx, voice_idx, item_idx) -> (dm, df_ticks)
        let mut slur_next: BTreeMap<(usize, usize, usize), (i64, i64)> = BTreeMap::new();
        let mut slur_prev: BTreeMap<(usize, usize, usize), (i64, i64)> = BTreeMap::new();

        let n_voices = grouped.first().map(|v| v.len()).unwrap_or(0);
        for vi in 0..n_voices {
            let mut open_ties: BTreeMap<i32, (usize, usize, usize, i64)> = BTreeMap::new();
            let mut open_slur: Option<(usize, usize, i64)> = None;
            for (m, voices) in grouped.iter().enumerate() {
                let Some((_, items)) = voices.get(vi) else {
                    continue;
                };
                for (ii, item) in items.iter().enumerate() {
                    if item.is_rest {
                        continue;
                    }
                    let has_slur_stop = item.members.iter().any(|s| s.note.slur_stop);
                    let has_slur_start = item.members.iter().any(|s| s.note.slur_start);
                    if has_slur_stop {
                        if let Some((sm, si, soff)) = open_slur.take() {
                            let dm = m as i64 - sm as i64;
                            let df = item.off_ticks - soff;
                            slur_next.insert((sm, vi, si), (dm, df));
                            slur_prev.insert((m, vi, ii), (-dm, -df));
                        }
                    }
                    if has_slur_start {
                        open_slur = Some((m, ii, item.off_ticks));
                    }
                    for (ni, seg) in item.members.iter().enumerate() {
                        let (t_start, t_stop) = tie_elements(seg);
                        if t_stop {
                            if let Some((sm, si, sn, soff)) = open_ties.remove(&seg.note.midi) {
                                let dm = m as i64 - sm as i64;
                                let df = item.off_ticks - soff;
                                tie_next.insert((sm, vi, si, sn), (dm, df));
                                tie_prev.insert((m, vi, ii, ni), (-dm, -df));
                            }
                        }
                        if t_start {
                            open_ties.insert(seg.note.midi, (m, ii, ni, item.off_ticks));
                        }
                    }
                }
            }
        }

        // Pair measure-scoped hairpins across measures (on voice 0).
        // hp_start: measure_idx -> (subtype: 0=cresc|1=dim, dm, df_ticks)
        // hp_stop_start: measure_idx -> Vec<(dm, df_ticks)> (closed at start of measure)
        // hp_stop_end: measure_idx -> Vec<(dm, df_ticks)> (closed at end of measure)
        let mut hp_start: BTreeMap<usize, (u8, i64, i64)> = BTreeMap::new();
        let mut hp_stop_start: BTreeMap<usize, Vec<(i64, i64)>> = BTreeMap::new();
        let mut hp_stop_end: BTreeMap<usize, Vec<(i64, i64)>> = BTreeMap::new();
        let mut open_hp: Option<(usize, u8)> = None;
        for m in 0..n_measures {
            if let Some(h) = measure_attr(score, m as u32 + 1).and_then(|a| a.hairpin.as_deref()) {
                match h {
                    "cresc" | "dim" => {
                        if let Some((sm, sub)) = open_hp.take() {
                            if m > sm {
                                let dm = m as i64 - sm as i64;
                                hp_start.insert(sm, (sub, dm, 0));
                                hp_stop_start.entry(m).or_default().push((-dm, 0));
                            }
                        }
                        let sub = if h == "cresc" { 0 } else { 1 };
                        open_hp = Some((m, sub));
                    }
                    _ => {
                        if let Some((sm, sub)) = open_hp.take() {
                            if m > sm {
                                let dm = m as i64 - sm as i64;
                                hp_start.insert(sm, (sub, dm, 0));
                                hp_stop_start.entry(m).or_default().push((-dm, 0));
                            } else {
                                let mlen = windows
                                    .get(m)
                                    .map(|&(a, b)| b - a)
                                    .unwrap_or_else(|| score.measure_ticks());
                                hp_start.insert(sm, (sub, 0, mlen));
                                hp_stop_end.entry(m).or_default().push((0, -mlen));
                            }
                        }
                    }
                }
            }
        }
        if let Some((sm, sub)) = open_hp.take() {
            if n_measures > 0 {
                let last = n_measures - 1;
                let mlen = windows
                    .get(last)
                    .map(|&(a, b)| b - a)
                    .unwrap_or_else(|| score.measure_ticks());
                let dm = last as i64 - sm as i64;
                hp_start.insert(sm, (sub, dm, mlen));
                hp_stop_end.entry(last).or_default().push((-dm, -mlen));
            }
        }

        let mut cur_sig = (score.beats, score.beat_type);
        let mut cur_key = score.key_fifths;

        for (m, voices) in grouped.iter().enumerate() {
            let attrs = measure_attr(score, m as u32 + 1);
            if let Some(a) = attrs {
                if let Some(t) = a.time {
                    cur_sig = (t.beats, t.beat_type);
                }
                if let Some(k) = a.key_fifths {
                    cur_key = k;
                }
            }
            let mlen = windows
                .get(m)
                .map(|&(a, b)| b - a)
                .unwrap_or_else(|| score.measure_ticks());

            out.push_str("      <Measure>\n");
            if attrs.and_then(|a| a.barline_left.as_deref()) == Some("repeat-left") {
                out.push_str("        <startRepeat/>\n");
            }
            if attrs.and_then(|a| a.barline_right.as_deref()) == Some("repeat-right") {
                out.push_str("        <endRepeat>2</endRepeat>\n");
            }

            for (vi, (_voice, items)) in voices.iter().enumerate() {
                out.push_str("        <voice>\n");
                if vi == 0 {
                    // Opening or mid-stream Clef, KeySig, TimeSig belong inside voice 0.
                    if m == 0 {
                        let (clef_s, clef_l, clef_oct) = match attrs {
                            Some(a) => (
                                a.clef_sign.unwrap_or('G'),
                                a.clef_line.unwrap_or(2),
                                a.clef_octave_change,
                            ),
                            None => ('G', 2, None),
                        };
                        let ct = mscx_clef_type(clef_s, clef_l, clef_oct);
                        out.push_str("          <Clef>\n");
                        let _ =
                            writeln!(out, "            <concertClefType>{ct}</concertClefType>");
                        let _ = writeln!(
                            out,
                            "            <transposingClefType>{ct}</transposingClefType>"
                        );
                        out.push_str("            <isHeader>1</isHeader>\n");
                        out.push_str("            </Clef>\n");
                        out.push_str("          <KeySig>\n");
                        let _ = writeln!(out, "            <concertKey>{cur_key}</concertKey>");
                        out.push_str("            </KeySig>\n");
                        out.push_str("          <TimeSig>\n");
                        let _ = writeln!(out, "            <sigN>{}</sigN>", cur_sig.0);
                        let _ = writeln!(out, "            <sigD>{}</sigD>", cur_sig.1);
                        out.push_str("            </TimeSig>\n");
                    } else if let Some(a) = attrs {
                        if a.clef_sign.is_some()
                            || a.clef_line.is_some()
                            || a.clef_octave_change.is_some()
                        {
                            let ct = mscx_clef_type(
                                a.clef_sign.unwrap_or('G'),
                                a.clef_line.unwrap_or(2),
                                a.clef_octave_change,
                            );
                            out.push_str("          <Clef>\n");
                            let _ = writeln!(
                                out,
                                "            <concertClefType>{ct}</concertClefType>"
                            );
                            let _ = writeln!(
                                out,
                                "            <transposingClefType>{ct}</transposingClefType>"
                            );
                            out.push_str("            </Clef>\n");
                        }
                        if a.key_fifths.is_some() {
                            out.push_str("          <KeySig>\n");
                            let _ = writeln!(out, "            <concertKey>{cur_key}</concertKey>");
                            out.push_str("            </KeySig>\n");
                        }
                        if a.time.is_some() {
                            out.push_str("          <TimeSig>\n");
                            let _ = writeln!(out, "            <sigN>{}</sigN>", cur_sig.0);
                            let _ = writeln!(out, "            <sigD>{}</sigD>", cur_sig.1);
                            out.push_str("            </TimeSig>\n");
                        }
                    }

                    if let Some(d) = attrs.and_then(|a| a.dynamic.as_deref()) {
                        out.push_str("          <Dynamic>\n            <subtype>");
                        write_escaped_xml(&mut out, d);
                        out.push_str("</subtype>\n            </Dynamic>\n");
                    }
                    if let Some(stops) = hp_stop_start.get(&m) {
                        for &(dm, df) in stops {
                            out.push_str("          <Spanner type=\"HairPin\">\n");
                            out.push_str("            <prev>\n");
                            write_mscx_location_xml(
                                &mut out,
                                "              ",
                                dm,
                                df,
                                score.divisions,
                            );
                            out.push_str("              </prev>\n");
                            out.push_str("            </Spanner>\n");
                        }
                    }
                    if let Some(&(sub, dm, df)) = hp_start.get(&m) {
                        out.push_str("          <Spanner type=\"HairPin\">\n");
                        out.push_str("            <HairPin>\n");
                        let _ = writeln!(out, "              <subtype>{sub}</subtype>");
                        out.push_str("              </HairPin>\n");
                        out.push_str("            <next>\n");
                        write_mscx_location_xml(
                            &mut out,
                            "              ",
                            dm,
                            df,
                            score.divisions,
                        );
                        out.push_str("              </next>\n");
                        out.push_str("            </Spanner>\n");
                    }
                }

                for (ii, item) in items.iter().enumerate() {
                    let has_fermata = item
                        .members
                        .iter()
                        .any(|s| s.note.articulations.iter().any(|a| a == "fermata"));
                    if has_fermata {
                        out.push_str("          <Fermata>\n");
                        out.push_str("            <subtype>fermataAbove</subtype>\n");
                        out.push_str("            </Fermata>\n");
                    }

                    if item.is_rest {
                        if item.grace {
                            // MuseScore does not support grace rests; skip.
                            continue;
                        }
                        out.push_str("          <Rest>\n");
                        if items.len() == 1 && item.dur_ticks == mlen {
                            out.push_str("            <durationType>measure</durationType>\n");
                            let _ = writeln!(
                                out,
                                "            <duration>{}/{}</duration>",
                                cur_sig.0, cur_sig.1
                            );
                        } else {
                            let (dtype, dots) = mscx_duration_type(item.dur_ticks, score.divisions);
                            if dots > 0 {
                                let _ = writeln!(out, "            <dots>{dots}</dots>");
                            }
                            let _ =
                                writeln!(out, "            <durationType>{dtype}</durationType>");
                        }
                        out.push_str("            </Rest>\n");
                        continue;
                    }

                    let (dtype, dots) = mscx_duration_type(item.dur_ticks, score.divisions);
                    out.push_str("          <Chord>\n");
                    if dots > 0 {
                        let _ = writeln!(out, "            <dots>{dots}</dots>");
                    }
                    let _ = writeln!(out, "            <durationType>{dtype}</durationType>");
                    if item.grace {
                        out.push_str("            <appoggiatura/>\n");
                    }
                    if let Some(ly) = item.members.iter().find_map(|s| s.note.lyric.as_deref()) {
                        out.push_str("            <Lyrics>\n              <text>");
                        write_escaped_xml(&mut out, ly);
                        out.push_str("</text>\n              </Lyrics>\n");
                    }
                    if let Some(&(dm, df)) = slur_prev.get(&(m, vi, ii)) {
                        out.push_str("            <Spanner type=\"Slur\">\n");
                        out.push_str("              <prev>\n");
                        write_mscx_location_xml(
                            &mut out,
                            "                ",
                            dm,
                            df,
                            score.divisions,
                        );
                        out.push_str("                </prev>\n");
                        out.push_str("              </Spanner>\n");
                    }
                    if let Some(&(dm, df)) = slur_next.get(&(m, vi, ii)) {
                        out.push_str("            <Spanner type=\"Slur\">\n");
                        out.push_str("              <Slur>\n");
                        out.push_str("                </Slur>\n");
                        out.push_str("              <next>\n");
                        write_mscx_location_xml(
                            &mut out,
                            "                ",
                            dm,
                            df,
                            score.divisions,
                        );
                        out.push_str("                </next>\n");
                        out.push_str("              </Spanner>\n");
                    }
                    let mut emitted_arts = 0u8;
                    for s in &item.members {
                        for a in &s.note.articulations {
                            let (bit, sub) = match a.as_str() {
                                "stacc" => (1u8 << 0, "articStaccatoAbove"),
                                "accent" => (1u8 << 1, "articAccentAbove"),
                                "tenuto" => (1u8 << 2, "articTenutoAbove"),
                                _ => continue,
                            };
                            if emitted_arts & bit == 0 {
                                emitted_arts |= bit;
                                out.push_str("            <Articulation>\n              <subtype>");
                                out.push_str(sub);
                                out.push_str("</subtype>\n              </Articulation>\n");
                            }
                        }
                    }
                    for (ni, s) in item.members.iter().enumerate() {
                        out.push_str("            <Note>\n");
                        if let Some(&(dm, df)) = tie_prev.get(&(m, vi, ii, ni)) {
                            out.push_str("              <Spanner type=\"Tie\">\n");
                            out.push_str("                <prev>\n");
                            write_mscx_location_xml(
                                &mut out,
                                "                  ",
                                dm,
                                df,
                                score.divisions,
                            );
                            out.push_str("                  </prev>\n");
                            out.push_str("                </Spanner>\n");
                        }
                        if let Some(&(dm, df)) = tie_next.get(&(m, vi, ii, ni)) {
                            out.push_str("              <Spanner type=\"Tie\">\n");
                            out.push_str("                <Tie>\n");
                            out.push_str("                  </Tie>\n");
                            out.push_str("                <next>\n");
                            write_mscx_location_xml(
                                &mut out,
                                "                  ",
                                dm,
                                df,
                                score.divisions,
                            );
                            out.push_str("                  </next>\n");
                            out.push_str("                </Spanner>\n");
                        }
                        let _ = writeln!(out, "              <pitch>{}</pitch>", s.note.midi);
                        let _ = writeln!(
                            out,
                            "              <tpc>{}</tpc>",
                            tpc(s.note.step, s.note.alter)
                        );
                        out.push_str("              </Note>\n");
                    }
                    out.push_str("            </Chord>\n");
                }

                if vi == 0 {
                    if let Some(stops) = hp_stop_end.get(&m) {
                        for &(dm, df) in stops {
                            out.push_str("          <Spanner type=\"HairPin\">\n");
                            out.push_str("            <prev>\n");
                            write_mscx_location_xml(
                                &mut out,
                                "              ",
                                dm,
                                df,
                                score.divisions,
                            );
                            out.push_str("              </prev>\n");
                            out.push_str("            </Spanner>\n");
                        }
                    }
                    match attrs.and_then(|a| a.barline_right.as_deref()) {
                        Some("light-heavy") => {
                            out.push_str("          <BarLine>\n");
                            out.push_str("            <subtype>end</subtype>\n");
                            out.push_str("            </BarLine>\n");
                        }
                        Some("double") => {
                            out.push_str("          <BarLine>\n");
                            out.push_str("            <subtype>double</subtype>\n");
                            out.push_str("            </BarLine>\n");
                        }
                        _ => {}
                    }
                }
                out.push_str("          </voice>\n");
            }
            out.push_str("        </Measure>\n");
        }
        out.push_str("      </Staff>\n");
    }
    out.push_str("    </Score>\n");
    out.push_str("  </museScore>\n");
    out
}

/// File stem for the score inside `.mscz`: ASCII alphanumerics, `-`, `_`.
pub fn score_stem(title: &str) -> String {
    let mut s: String = title
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    while s.contains("--") {
        s = s.replace("--", "-");
    }
    if s.is_empty() {
        s = "score".to_string();
    }
    s
}

fn container_xml(mscx_name: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<container>\n  <rootfiles>\n    <rootfile full-path=\"{mscx_name}\"/>\n    <rootfile full-path=\"score_style.mss\"/>\n  </rootfiles>\n</container>\n"
    )
}

fn score_style_mss() -> &'static str {
    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<museScore version=\"4.20\">\n  <Style>\n    <Spatium>1.74978</Spatium>\n  </Style>\n</museScore>\n"
}

// ---------------------------------------------------------------------------
// SER (Symbol Error Rate)
// ---------------------------------------------------------------------------

/// Symbol Error Rate report: Levenshtein distance over symbol sequences,
/// normalised by the reference length (`ser = distance / ref_len`).
///
/// A symbol is one note/rest event rendered as
/// `"p{part}:R|m{midi}:d{dur_ticks}:v{voice}"`, in score order (parts in
/// input order, then `(start_ticks, voice, midi)`). Rests count as symbols,
/// so a dropped rest is one deletion. Values above 1.0 are possible when
/// the hypothesis inserts more symbols than the reference holds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerReport {
    pub distance: usize,
    pub ref_len: usize,
    pub hyp_len: usize,
    pub ser: f64,
}

/// Levenshtein edit distance over symbol sequences (insert/delete/substitute
/// cost 1). Time O(a*b), memory O(min(a,b)).
pub fn levenshtein(a: &[String], b: &[String]) -> usize {
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    // Keep the shorter sequence on the inner axis.
    let (short, long) = if b.len() < a.len() { (b, a) } else { (a, b) };
    let mut prev: Vec<usize> = (0..=short.len()).collect();
    let mut curr = vec![0usize; short.len() + 1];
    for (i, l) in long.iter().enumerate() {
        curr[0] = i + 1;
        for (j, s) in short.iter().enumerate() {
            let sub = prev[j] + usize::from(l != s);
            let ins = curr[j] + 1;
            let del = prev[j + 1] + 1;
            curr[j + 1] = sub.min(ins).min(del);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[short.len()]
}

/// Canonical symbol sequence for a parsed [`Score`].
pub fn score_symbols(score: &Score) -> Vec<String> {
    let mut out = Vec::new();
    for (pi, part) in score.parts.iter().enumerate() {
        for g in &part.groups {
            for m in &g.members {
                if m.rest {
                    out.push(format!("p{pi}:R:d{}:v{}", g.dur_ticks, g.voice));
                } else {
                    out.push(format!("p{pi}:m{}:d{}:v{}", m.midi, g.dur_ticks, g.voice));
                }
            }
        }
    }
    out
}

/// SER over two symbol sequences.
pub fn ser_for_symbols(reference: &[String], hypothesis: &[String]) -> SerReport {
    let distance = levenshtein(reference, hypothesis);
    let ser = if reference.is_empty() {
        if hypothesis.is_empty() {
            0.0
        } else {
            hypothesis.len() as f64
        }
    } else {
        distance as f64 / reference.len() as f64
    };
    SerReport {
        distance,
        ref_len: reference.len(),
        hyp_len: hypothesis.len(),
        ser,
    }
}

/// Parse both inputs with [`parse_score`] (canonical tick order) and report
/// SER of hypothesis against reference, with the two symbol sequences.
pub fn ser_for_inputs(
    reference: &ScoreInput,
    hypothesis: &ScoreInput,
) -> Result<(SerReport, Vec<String>, Vec<String>), String> {
    let (ref_score, _) = parse_score(reference).map_err(|e| format!("reference: {e}"))?;
    let (hyp_score, _) = parse_score(hypothesis).map_err(|e| format!("hypothesis: {e}"))?;
    let ref_syms = score_symbols(&ref_score);
    let hyp_syms = score_symbols(&hyp_score);
    let report = ser_for_symbols(&ref_syms, &hyp_syms);
    Ok((report, ref_syms, hyp_syms))
}

/// Pack rendered `.mscx` into `.mscz` bytes (deflated zip with
/// `META-INF/container.xml`, `<stem>.mscx`, and `score_style.mss`).
pub fn pack_mscz(score: &Score, mscx: &str) -> Result<Vec<u8>, String> {
    let stem = score_stem(&score.title);
    let mscx_name = format!("{stem}.mscx");
    let mut buf = Cursor::new(Vec::with_capacity(mscx.len() / 4 + 512));
    {
        let mut zip = zip::ZipWriter::new(&mut buf);
        let stored = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        let deflated = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        zip.start_file("META-INF/container.xml", stored)
            .map_err(|e| format!("zip container.xml: {e}"))?;
        zip.write_all(container_xml(&mscx_name).as_bytes())
            .map_err(|e| format!("zip container.xml: {e}"))?;
        zip.start_file(mscx_name, deflated)
            .map_err(|e| format!("zip score: {e}"))?;
        zip.write_all(mscx.as_bytes())
            .map_err(|e| format!("zip score: {e}"))?;
        zip.start_file("score_style.mss", stored)
            .map_err(|e| format!("zip score_style.mss: {e}"))?;
        zip.write_all(score_style_mss().as_bytes())
            .map_err(|e| format!("zip score_style.mss: {e}"))?;
        zip.finish().map_err(|e| format!("zip finish: {e}"))?;
    }
    Ok(buf.into_inner())
}

/// Render a [`Score`] to `.mscx` and pack it directly into `.mscz` bytes in
/// one call.
pub fn pack_mscz_bytes(score: &Score, losses: &mut LossTable) -> Result<Vec<u8>, String> {
    let mscx = to_mscx(score, losses);
    pack_mscz(score, &mscx)
}

/// Extracted contents of an `.mscz` archive returned by [`read_mscz`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MsczContents {
    pub root_mscx_path: String,
    pub mscx: String,
    pub style_mss: Option<String>,
    pub entries: Vec<String>,
}

/// Maximum uncompressed size (64 MiB) allowed when reading an entry from an
/// `.mscz` archive. Protects untrusted callers (including web/WASM) against
/// zip-bomb OOMs.
pub const MAX_MSCZ_ENTRY_BYTES: u64 = 64 * 1024 * 1024;

/// Read a zip entry into a UTF-8 `String`, refusing entries whose declared or
/// actual decompressed size exceeds `max_bytes` before allocating unboundedly.
fn read_bounded_utf8<R: Read>(
    reader: R,
    declared_size: u64,
    name: &str,
    max_bytes: u64,
) -> Result<String, String> {
    if declared_size > max_bytes {
        return Err(format!(
            "{name:?} exceeds maximum uncompressed size ({declared_size} > {max_bytes} bytes)"
        ));
    }
    let mut buf = Vec::with_capacity(declared_size.min(1024 * 1024) as usize);
    reader
        .take(max_bytes + 1)
        .read_to_end(&mut buf)
        .map_err(|e| format!("cannot read {name:?}: {e}"))?;
    if buf.len() as u64 > max_bytes {
        return Err(format!(
            "{name:?} exceeds maximum uncompressed size ({max_bytes} bytes)"
        ));
    }
    String::from_utf8(buf).map_err(|e| format!("cannot read {name:?}: {e}"))
}

/// Inspect and extract the root `.mscx` score, optional `.mss` style sheet,
/// and archive entry list from `.mscz` zip bytes.
pub fn read_mscz(bytes: &[u8]) -> Result<MsczContents, String> {
    let mut zip =
        zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| format!("not a readable zip: {e}"))?;
    let entries: Vec<String> = zip.file_names().map(str::to_string).collect();

    let container = {
        let f = zip
            .by_name("META-INF/container.xml")
            .map_err(|_| "missing META-INF/container.xml".to_string())?;
        let sz = f.size();
        read_bounded_utf8(f, sz, "META-INF/container.xml", MAX_MSCZ_ENTRY_BYTES)?
    };

    let mut rootfiles: Vec<String> = Vec::new();
    let mut search = 0usize;
    while let Some(rel) = container[search..].find("full-path=\"") {
        let s = search + rel + "full-path=\"".len();
        let e = container[s..]
            .find('"')
            .map(|q| s + q)
            .unwrap_or(container.len());
        rootfiles.push(container[s..e].to_string());
        search = (e + 1).min(container.len());
    }

    let root_mscx_path = rootfiles
        .iter()
        .find(|p| p.ends_with(".mscx"))
        .cloned()
        .ok_or_else(|| format!("container.xml has no .mscx rootfile (found {rootfiles:?})"))?;

    let mscx = {
        let f = zip
            .by_name(&root_mscx_path)
            .map_err(|_| format!("rootfile {root_mscx_path:?} missing from zip"))?;
        let sz = f.size();
        read_bounded_utf8(f, sz, &root_mscx_path, MAX_MSCZ_ENTRY_BYTES)?
    };

    let style_path = rootfiles
        .iter()
        .find(|p| p.ends_with(".mss"))
        .cloned()
        .or_else(|| {
            entries
                .iter()
                .find(|p| p.as_str() == "score_style.mss" || p.ends_with(".mss"))
                .cloned()
        });

    let style_mss = match style_path {
        Some(path) => match zip.by_name(&path) {
            Ok(f) => {
                let sz = f.size();
                Some(read_bounded_utf8(f, sz, &path, MAX_MSCZ_ENTRY_BYTES)?)
            }
            Err(_) => None,
        },
        None => None,
    };

    Ok(MsczContents {
        root_mscx_path,
        mscx,
        style_mss,
        entries,
    })
}

// ---------------------------------------------------------------------------
// Validators
// ---------------------------------------------------------------------------

/// Validate a raw [`ScoreInput`] before conversion.
/// Errors: structural problems that make conversion fail; warnings: things
/// conversion will normalise (unsorted onsets, explicit rests, gaps).
pub fn validate_notes(input: &ScoreInput) -> Vec<Issue> {
    let mut out = Vec::new();
    if input.divisions == 0 {
        out.push(error(
            "notes-divisions",
            "divisions must be positive".to_string(),
        ));
    }
    let beats = input.time.map(|t| t.beats).unwrap_or(4);
    let beat_type = input.time.map(|t| t.beat_type).unwrap_or(4);
    if beats == 0 || beat_type == 0 {
        out.push(error(
            "notes-time",
            "time beats and beat_type must be positive".to_string(),
        ));
    } else if !matches!(beat_type, 1 | 2 | 4 | 8 | 16 | 32) {
        out.push(error(
            "notes-time",
            format!("unsupported beat_type {beat_type}"),
        ));
    }
    let parts: Vec<(&str, &[NoteInput])> = if !input.parts.is_empty() {
        input
            .parts
            .iter()
            .map(|p| (p.name.as_str(), p.notes.as_slice()))
            .collect()
    } else {
        vec![(
            input.part_name.as_deref().unwrap_or("Part 1"),
            input.notes.as_slice(),
        )]
    };
    if parts.iter().all(|(_, n)| n.is_empty()) {
        out.push(error("notes-empty", "score has no notes".to_string()));
        return out;
    }
    for (pname, notes) in &parts {
        let mut prev_onset = -1.0f64;
        let mut seen_gap = false;
        let mut cursor = 0.0f64;
        for (i, n) in notes.iter().enumerate() {
            let here = format!("part {pname:?} note {i}");
            if !n.onset.is_finite() || !n.dur.is_finite() {
                out.push(error(
                    "notes-finite",
                    format!("{here}: onset/dur must be finite"),
                ));
                continue;
            }
            if n.onset < 0.0 {
                out.push(error(
                    "notes-onset",
                    format!("{here}: negative onset {}", n.onset),
                ));
            }
            if n.dur <= 0.0 {
                out.push(error(
                    "notes-duration",
                    format!("{here}: non-positive dur {}", n.dur),
                ));
            }
            if n.voice == 0 {
                out.push(error("notes-voice", format!("{here}: voice must be >= 1")));
            }
            if n.onset < prev_onset {
                out.push(warning(
                    "notes-unsorted",
                    format!("{here}: onsets not in order (converter sorts)"),
                ));
            }
            prev_onset = n.onset;
            if n.onset > cursor + 1e-9 {
                seen_gap = true;
            }
            cursor = cursor.max(n.onset + n.dur.max(0.0));
            let is_rest = n.rest
                || n.pitch
                    .as_ref()
                    .map(|p| p.trim().is_empty())
                    .unwrap_or(true);
            if !is_rest {
                match parse_pitch(n.pitch.as_ref().unwrap()) {
                    Ok((_, _, _, midi, cents)) => {
                        if !(0..=127).contains(&midi) {
                            out.push(error("notes-range", format!("{here}: out of MIDI range")));
                        }
                        if cents.is_some() {
                            out.push(warning(
                                "notes-microtone",
                                format!("{here}: cents suffix will be rounded to MIDI {midi}"),
                            ));
                        }
                    }
                    Err(e) => out.push(error("notes-pitch", format!("{here}: {e}"))),
                }
            }
        }
        if seen_gap {
            out.push(warning(
                "notes-gaps",
                format!("part {pname:?}: onset gaps will be filled with rests"),
            ));
        }
    }
    out
}

fn read_tag_value(xml: &str, start: usize, tag: &str) -> Option<(i64, usize)> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let s = xml[start..].find(&open)? + start + open.len();
    let e = xml[s..].find(&close)? + s;
    let v = xml[s..e].trim().parse::<i64>().ok()?;
    Some((v, e + close.len()))
}

/// Return the largest UTF-8 char boundary in `s` at or before `idx`.
fn floor_char_boundary(s: &str, idx: usize) -> usize {
    let mut i = idx.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Find the byte offset of the next `<tag>` or `<tag ...>` opening tag in `s`.
fn find_open_tag(s: &str, tag: &str) -> Option<usize> {
    let needle = format!("<{tag}");
    let mut off = 0;
    while let Some(rel) = s[off..].find(&needle) {
        let pos = off + rel;
        let after = pos + needle.len();
        if let Some(&b) = s.as_bytes().get(after) {
            if b == b'>' || b.is_ascii_whitespace() {
                return Some(pos);
            }
        }
        off = after;
    }
    None
}

/// Validate a MusicXML document (ours or foreign):
/// declaration, `score-partwise version="3.1"` or `"4.0"`, per-measure
/// duration sums honouring `<backup>`/`<forward>`, positive durations, pitched
/// notes with step+octave, and no unescaped `&` in text.
pub fn validate_musicxml(xml: &str) -> Vec<Issue> {
    let mut out = Vec::new();
    if !xml.starts_with("<?xml") {
        out.push(error(
            "xml-declaration",
            "missing <?xml declaration".to_string(),
        ));
    }
    match xml.find("<score-partwise") {
        None => {
            out.push(error(
                "xml-root",
                "missing <score-partwise> root".to_string(),
            ));
            return out;
        }
        Some(p) => {
            let head_end = floor_char_boundary(xml, p + 200);
            let head = &xml[p..head_end];
            if !head.contains("version=\"3.1\"") && !head.contains("version=\"4.0\"") {
                out.push(error(
                    "xml-version",
                    "score-partwise version must be \"3.1\" or \"4.0\" for this pipeline"
                        .to_string(),
                ));
            }
        }
    }
    // Crude unescaped-& scan: every '&' must start a known entity.
    let bytes = xml.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'&' {
            let tail_end = floor_char_boundary(xml, i + 8);
            let tail = &xml[i..tail_end];
            if !(tail.starts_with("&amp;")
                || tail.starts_with("&lt;")
                || tail.starts_with("&gt;")
                || tail.starts_with("&quot;")
                || tail.starts_with("&apos;"))
            {
                out.push(error("xml-escape", format!("unescaped '&' at byte {i}")));
                break;
            }
        }
        i += 1;
    }
    // Per-measure duration simulation.
    let mut divisions = read_tag_value(xml, 0, "divisions")
        .map(|(v, _)| v)
        .unwrap_or(1);
    if divisions <= 0 {
        out.push(error(
            "xml-divisions",
            "divisions must be positive".to_string(),
        ));
        return out;
    }
    let mut beats = read_tag_value(xml, 0, "beats").map(|(v, _)| v).unwrap_or(4);
    let mut beat_type = read_tag_value(xml, 0, "beat-type")
        .map(|(v, _)| v)
        .unwrap_or(4);
    if beats <= 0 || beat_type <= 0 {
        out.push(error(
            "xml-time",
            "time beats/beat-type must be positive".to_string(),
        ));
        return out;
    }
    let mut expected = beats.saturating_mul(divisions).saturating_mul(4) / beat_type;
    let mut pos = 0;
    let mut mnum = 0;
    let mut note_count = 0;
    while let Some(mp) = xml[pos..].find("<measure") {
        mnum += 1;
        let mstart = pos + mp;
        let mend = match xml[mstart..].find("</measure>") {
            Some(e) => mstart + e,
            None => {
                out.push(error(
                    "xml-measure",
                    format!("measure {mnum}: missing </measure>"),
                ));
                break;
            }
        };
        let body = &xml[mstart..mend];
        if let Some((md, _)) = read_tag_value(body, 0, "divisions") {
            if md > 0 {
                divisions = md;
                expected = beats.saturating_mul(divisions).saturating_mul(4) / beat_type;
            } else {
                out.push(error(
                    "xml-divisions",
                    format!("measure {mnum}: divisions must be positive (got {md})"),
                ));
            }
        }
        if let (Some((mb, _)), Some((mbt, _))) = (
            read_tag_value(body, 0, "beats"),
            read_tag_value(body, 0, "beat-type"),
        ) {
            if mb > 0 && mbt > 0 {
                beats = mb;
                beat_type = mbt;
                expected = beats.saturating_mul(divisions).saturating_mul(4) / beat_type;
            }
        }
        let mut t = 0i64;
        let mut k = 0;
        let mut ctx = 0u8; // 0 none, 1 note, 2 backup, 3 forward, 4 attributes(skip)
        let mut chord_tone = false;
        while k < body.len() {
            if body.as_bytes()[k] != b'<' {
                match body[k..].find('<') {
                    Some(rel) => k += rel,
                    None => break,
                }
                continue;
            }
            if body[k..].starts_with("<note>") || body[k..].starts_with("<note ") {
                ctx = 1;
                chord_tone = false;
                k += 6;
            } else if body[k..].starts_with("<chord") && ctx == 1 {
                // A chord tone shares the previous note's onset: per the
                // MusicXML spec its duration does not advance time.
                chord_tone = true;
                k += 6;
            } else if body[k..].starts_with("<backup>") {
                ctx = 2;
                k += 8;
            } else if body[k..].starts_with("<forward>") {
                ctx = 3;
                k += 9;
            } else if body[k..].starts_with("<attributes>") {
                ctx = 4;
                k += 12;
            } else if body[k..].starts_with("</note>")
                || body[k..].starts_with("</backup>")
                || body[k..].starts_with("</forward>")
                || body[k..].starts_with("</attributes>")
            {
                ctx = 0;
                k += 1;
            } else if body[k..].starts_with("<divisions>") && ctx == 4 {
                if let Some((md, next)) = read_tag_value(body, k, "divisions") {
                    if md > 0 {
                        divisions = md;
                        expected = beats.saturating_mul(divisions).saturating_mul(4) / beat_type;
                    }
                    k = next;
                } else {
                    k += 1;
                }
            } else if body[k..].starts_with("<duration>") {
                match read_tag_value(body, k, "duration") {
                    Some((v, next)) => {
                        if ctx == 4 {
                            // divisions inside attributes: not a time advance.
                        } else if v <= 0 {
                            out.push(error(
                                "xml-duration",
                                format!("measure {mnum}: non-positive duration {v}"),
                            ));
                        } else if ctx == 2 {
                            t = t.saturating_sub(v);
                        } else if ctx == 1 && chord_tone {
                            // Chord tone: shares onset, no time advance.
                        } else {
                            t = t.saturating_add(v);
                        }
                        k = next;
                    }
                    None => {
                        out.push(error(
                            "xml-duration",
                            format!("measure {mnum}: bad <duration>"),
                        ));
                        k += 1;
                    }
                }
            } else {
                k += 1;
            }
        }
        if t != expected {
            out.push(error(
                "xml-measure-sum",
                format!("measure {mnum}: durations sum to {t} ticks, expected {expected}"),
            ));
        }
        // Pitched-note completeness within this measure.
        let mut n = 0;
        let mut nn = 0;
        while let Some(p) = find_open_tag(&body[nn..], "note") {
            n += 1;
            let ns = nn + p;
            let ne = body[ns..]
                .find("</note>")
                .map(|e| ns + e)
                .unwrap_or(body.len());
            let nb = &body[ns..ne];
            note_count += 1;
            if !nb.contains("<rest") && !nb.contains("<rest/>") {
                if !(nb.contains("<step>") && nb.contains("<octave>")) {
                    out.push(error(
                        "xml-pitch",
                        format!("measure {mnum}: pitched note without step+octave"),
                    ));
                }
                // Grace notes correctly carry no `<duration>` (they steal
                // no time); requiring one would reject valid notation.
                if !nb.contains("<grace") && !nb.contains("<duration>") {
                    out.push(error(
                        "xml-duration",
                        format!("measure {mnum}: note without duration"),
                    ));
                }
            }
            if ne >= body.len() {
                break;
            }
            nn = ne + "</note>".len();
        }
        if n == 0 {
            out.push(warning(
                "xml-empty-measure",
                format!("measure {mnum}: no notes"),
            ));
        }
        pos = mend + "</measure>".len();
    }
    if mnum == 0 {
        out.push(error(
            "xml-nomeasures",
            "no <measure> elements found".to_string(),
        ));
    }
    if note_count == 0 {
        out.push(warning(
            "xml-nonotes",
            "no <note> elements found".to_string(),
        ));
    }
    out
}

/// Validate `.mscz` bytes: readable zip, `META-INF/container.xml` present,
/// rootfile entry present and ending in `.mscx`, score entry parseable with
/// `<museScore>` root and at least one `<Measure>`.
pub fn validate_mscz(data: &[u8]) -> Vec<Issue> {
    let mut out = Vec::new();
    let mut zip = match zip::ZipArchive::new(Cursor::new(data)) {
        Ok(z) => z,
        Err(e) => {
            out.push(error("mscz-zip", format!("not a readable zip: {e}")));
            return out;
        }
    };
    let container = match zip.by_name("META-INF/container.xml") {
        Ok(f) => {
            let sz = f.size();
            match read_bounded_utf8(f, sz, "META-INF/container.xml", MAX_MSCZ_ENTRY_BYTES) {
                Ok(s) => s,
                Err(e) => {
                    out.push(error("mscz-container", e));
                    String::new()
                }
            }
        }
        Err(_) => {
            out.push(error(
                "mscz-container",
                "missing META-INF/container.xml".to_string(),
            ));
            String::new()
        }
    };
    if !container.is_empty() {
        let mut rootfiles: Vec<String> = Vec::new();
        let mut search = 0usize;
        while let Some(rel) = container[search..].find("full-path=\"") {
            let s = search + rel + "full-path=\"".len();
            let e = container[s..]
                .find('"')
                .map(|q| s + q)
                .unwrap_or(container.len());
            rootfiles.push(container[s..e].to_string());
            search = (e + 1).min(container.len());
        }
        if rootfiles.is_empty() {
            out.push(error(
                "mscz-container",
                "container.xml has no rootfile full-path".to_string(),
            ));
        } else {
            let mscx_target = rootfiles.iter().find(|p| p.ends_with(".mscx")).cloned();
            for rf in &rootfiles {
                if zip.index_for_name(rf).is_none() {
                    out.push(error(
                        "mscz-score",
                        format!("container points at {rf:?} but it is not in the zip"),
                    ));
                } else if rf.ends_with(".mss") {
                    if let Ok(f) = zip.by_name(rf) {
                        let sz = f.size();
                        if let Err(e) = read_bounded_utf8(f, sz, rf, MAX_MSCZ_ENTRY_BYTES) {
                            out.push(error("mscz-style", e));
                        }
                    }
                }
            }
            match mscx_target {
                None => out.push(error(
                    "mscz-container",
                    format!("container.xml has no .mscx rootfile (found {rootfiles:?})"),
                )),
                Some(target) => {
                    if let Ok(f) = zip.by_name(&target) {
                        let sz = f.size();
                        match read_bounded_utf8(f, sz, &target, MAX_MSCZ_ENTRY_BYTES) {
                            Err(e) => {
                                out.push(error("mscz-score", e));
                            }
                            Ok(s) => {
                                if !s.contains("<museScore") {
                                    out.push(error(
                                        "mscz-score",
                                        format!("{target}: missing <museScore> root"),
                                    ));
                                }
                                if !s.contains("<Measure>") && !s.contains("<Measure ") {
                                    out.push(error(
                                        "mscz-score",
                                        format!("{target}: no <Measure> found"),
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Example + tests
// ---------------------------------------------------------------------------

/// Small two-bar example input (also used by the `example` CLI subcommand).
pub fn example_input() -> ScoreInput {
    ScoreInput {
        title: Some("Two-Voice Prelude".to_string()),
        composer: Some("Example".to_string()),
        divisions: SCORE_DIVISIONS,
        time: Some(TimeSig {
            beats: 4,
            beat_type: 4,
        }),
        key_fifths: 0,
        parts: vec![PartInput {
            name: "Piano".to_string(),
            notes: vec![
                NoteInput::note("C4", 0.0, 1.0),
                NoteInput::note("E4", 1.0, 1.0),
                NoteInput::note("G4", 2.0, 1.0),
                NoteInput::note("C5", 3.0, 1.0),
                NoteInput::note("G4", 4.0, 1.5),
                NoteInput::note("E4", 5.5, 0.5),
                NoteInput::note("D4", 6.0, 1.0),
                NoteInput::note("C4", 7.0, 1.0),
            ],
        }],
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn simple_score() -> (Score, LossTable) {
        parse_score(&example_input()).expect("example parses")
    }

    #[test]
    fn pitch_shapes() {
        assert!(parse_pitch("C4").is_ok());
        assert!(parse_pitch("F#5").is_ok());
        assert!(parse_pitch("Bb3").is_ok());
        assert!(parse_pitch("Gx2").is_ok());
        assert!(parse_pitch("H4").is_err());
        assert!(parse_pitch("C").is_err());
        assert!(parse_pitch("C###4").is_err());
        let (_, _, _, midi, cents) = parse_pitch("A4+80c").unwrap();
        assert_eq!(cents, Some(80));
        assert_eq!(midi, 70); // A#4
    }

    #[test]
    fn quantize_common_values() {
        assert_eq!(quantize_duration(480, 480), ("quarter", 0, 480));
        assert_eq!(quantize_duration(720, 480), ("quarter", 1, 720));
        assert_eq!(quantize_duration(1920, 480), ("whole", 0, 1920));
    }

    #[test]
    fn musicxml_roundtrip_validates() {
        let (score, _) = simple_score();
        let mut losses = LossTable::default();
        let xml = to_musicxml(&score, &mut losses);
        assert!(xml.contains("<score-partwise version=\"3.1\">"));
        let issues = validate_musicxml(&xml);
        let errs: Vec<_> = issues.iter().filter(|i| i.severity == "error").collect();
        assert!(errs.is_empty(), "errors: {errs:?}");
    }

    #[test]
    fn simultaneous_notes_form_chord() {
        let input: ScoreInput = serde_json::from_value(serde_json::json!({
            "notes": [
                {"pitch": "C4", "onset": 0.0, "dur": 1.0},
                {"pitch": "E4", "onset": 0.0, "dur": 1.0}
            ]
        }))
        .unwrap();
        let (score, losses) = parse_score(&input).unwrap();
        assert_eq!(score.parts[0].groups.len(), 1);
        assert_eq!(score.parts[0].groups[0].members.len(), 2);
        assert!(losses.is_lossless());
        let mut l2 = LossTable::default();
        let xml = to_musicxml(&score, &mut l2);
        assert!(xml.contains("<chord/>"));
        assert!(!has_errors(&validate_musicxml(&xml)));
    }

    #[test]
    fn ties_span_barlines() {
        let input: ScoreInput = serde_json::from_value(serde_json::json!({
            "notes": [{"pitch": "C4", "onset": 3.0, "dur": 2.0}]
        }))
        .unwrap();
        let (score, _) = parse_score(&input).unwrap();
        let mut losses = LossTable::default();
        let xml = to_musicxml(&score, &mut losses);
        assert!(xml.contains("<tie type=\"start\"/>"));
        assert!(xml.contains("<tie type=\"stop\"/>"));
        let issues = validate_musicxml(&xml);
        assert!(!has_errors(&issues), "{issues:?}");
    }

    #[test]
    fn loss_table_records_microtone_and_clamp() {
        let input: ScoreInput = serde_json::from_value(serde_json::json!({
            "notes": [
                {"pitch": "A4+30c", "onset": 0.0, "dur": 1.0},
                {"pitch": "C4", "onset": 0.5, "dur": 1.0}
            ]
        }))
        .unwrap();
        let (_, losses) = parse_score(&input).unwrap();
        assert!(losses
            .entries
            .iter()
            .any(|e| e.kind == "pitch-microtone-rounded"));
        assert!(losses.entries.iter().any(|e| e.kind == "overlap-clamped"));
    }

    #[test]
    fn mscz_roundtrip_validates() {
        let (score, _) = simple_score();
        let mut losses = LossTable::default();
        let mscx = to_mscx(&score, &mut losses);
        let data = pack_mscz(&score, &mscx).expect("pack");
        let issues = validate_mscz(&data);
        assert!(!has_errors(&issues), "{issues:?}");
    }

    #[test]
    fn validators_catch_bad_input() {
        let bad: ScoreInput = serde_json::from_value(serde_json::json!({
            "notes": [{"pitch": "H9", "onset": -1.0, "dur": 0.0}]
        }))
        .unwrap();
        let issues = validate_notes(&bad);
        assert!(has_errors(&issues));
        assert!(validate_musicxml("<plumbing/>")
            .iter()
            .any(|i| i.severity == "error"));
        assert!(validate_mscz(b"nope").iter().any(|i| i.severity == "error"));
    }

    fn syms(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn ser_identical_is_zero() {
        let (score, _) = simple_score();
        let s = score_symbols(&score);
        let r = ser_for_symbols(&s, &s);
        assert_eq!(r.distance, 0);
        assert_eq!(r.ref_len, s.len());
        assert_eq!(r.ser, 0.0);
    }

    #[test]
    fn ser_counts_substitution_insertion_deletion() {
        let r = ser_for_symbols(&syms(&["a", "b", "c"]), &syms(&["a", "x", "c"]));
        assert_eq!(r.distance, 1);
        assert!((r.ser - 1.0 / 3.0).abs() < 1e-12);
        let ins = ser_for_symbols(&syms(&["a", "b"]), &syms(&["a", "x", "b"]));
        assert_eq!(ins.distance, 1);
        assert!((ins.ser - 0.5).abs() < 1e-12);
        let del = ser_for_symbols(&syms(&["a", "x", "b"]), &syms(&["a", "b"]));
        assert_eq!(del.distance, 1);
        assert!((del.ser - 1.0 / 3.0).abs() < 1e-12);
    }

    #[test]
    fn ser_empty_reference_edge() {
        let both = ser_for_symbols(&[], &[]);
        assert_eq!(both.ser, 0.0);
        let hyp_only = ser_for_symbols(&[], &syms(&["a", "b"]));
        assert_eq!(hyp_only.distance, 2);
        assert_eq!(hyp_only.ser, 2.0);
    }

    #[test]
    fn ser_for_inputs_uses_canonical_ticks() {
        // 0.0001 beats rounds to the same tick as 0.0, so SER is 0.
        let reference: ScoreInput = serde_json::from_value(serde_json::json!({
            "notes": [{"pitch": "C4", "onset": 0.0, "dur": 1.0}]
        }))
        .unwrap();
        let hypothesis: ScoreInput = serde_json::from_value(serde_json::json!({
            "notes": [{"pitch": "C4", "onset": 0.0001, "dur": 1.0}]
        }))
        .unwrap();
        let (report, _, _) = ser_for_inputs(&reference, &hypothesis).unwrap();
        assert_eq!(report.ser, 0.0);
        let wrong: ScoreInput = serde_json::from_value(serde_json::json!({
            "notes": [{"pitch": "D4", "onset": 0.0, "dur": 1.0}]
        }))
        .unwrap();
        let (report, _, _) = ser_for_inputs(&reference, &wrong).unwrap();
        assert_eq!(report.distance, 1);
        assert_eq!(report.ser, 1.0);
    }

    #[test]
    fn ms4_xml_structure_and_mscz_container() {
        let (score, _) = simple_score();
        let mut losses = LossTable::default();
        let mscx = to_mscx(&score, &mut losses);
        assert!(mscx.contains("<museScore version=\"4.20\">"));
        assert!(mscx.contains("<programVersion>4.2.0</programVersion>"));
        assert!(mscx.contains("<Part id=\"1\">"));
        assert!(mscx.contains("<Instrument>"));
        assert!(mscx.contains("<Staff id=\"1\">"));
        assert!(mscx.contains("<concertKey>0</concertKey>"));
        assert!(mscx.contains("<concertClefType>G</concertClefType>"));
        assert!(mscx.contains("<isHeader>1</isHeader>"));
        assert!(mscx.contains("<sigN>4</sigN>"));
        assert!(mscx.contains("<sigD>4</sigD>"));

        let data = pack_mscz(&score, &mscx).expect("pack");
        let issues = validate_mscz(&data);
        assert!(!has_errors(&issues), "{issues:?}");

        let mut zip = zip::ZipArchive::new(Cursor::new(&data)).expect("zip");
        assert!(zip.by_name("META-INF/container.xml").is_ok());
        assert!(zip.by_name("Two-Voice-Prelude.mscx").is_ok());
        assert!(zip.by_name("score_style.mss").is_ok());
        let mut container = String::new();
        std::io::Read::read_to_string(
            &mut zip.by_name("META-INF/container.xml").unwrap(),
            &mut container,
        )
        .unwrap();
        assert!(container.contains("full-path=\"Two-Voice-Prelude.mscx\""));
        assert!(container.contains("full-path=\"score_style.mss\""));
    }

    #[test]
    fn ms4_multivoice_clefs_keys_timesigs() {
        let input: ScoreInput = serde_json::from_value(serde_json::json!({
            "title": "Multi Voice",
            "divisions": 480,
            "time": {"beats": 4, "beat_type": 4},
            "key_fifths": 2,
            "parts": [{
                "name": "Piano",
                "notes": [
                    {"pitch": "C5", "onset": 0.0, "dur": 4.0, "voice": 1},
                    {"pitch": "C3", "onset": 0.0, "dur": 2.0, "voice": 2},
                    {"pitch": "G3", "onset": 2.0, "dur": 2.0, "voice": 2},
                    {"pitch": "D5", "onset": 4.0, "dur": 3.0, "voice": 1},
                    {"rest": true, "onset": 4.0, "dur": 3.0, "voice": 2}
                ]
            }],
            "measure_attrs": [
                {"measure": 1, "clef_sign": "G", "clef_line": 2, "clef_octave_change": -1},
                {"measure": 2, "clef_sign": "F", "clef_line": 4, "key_fifths": -1, "time": {"beats": 3, "beat_type": 4}}
            ]
        }))
        .unwrap();
        let (score, _) = parse_score(&input).unwrap();
        let mut losses = LossTable::default();
        let mscx = to_mscx(&score, &mut losses);

        // Two voices per measure -> 4 <voice> blocks across 2 measures.
        assert_eq!(mscx.matches("<voice>").count(), 4);
        // Header clef G8vb in m1, mid-stream clef F in m2 (only in voice 0).
        assert_eq!(mscx.matches("<Clef>").count(), 2);
        assert!(mscx.contains("<concertClefType>G8vb</concertClefType>"));
        assert!(mscx.contains("<concertClefType>F</concertClefType>"));
        // Opening key +2 and mid-stream key -1 (only in voice 0).
        assert_eq!(mscx.matches("<KeySig>").count(), 2);
        assert!(mscx.contains("<concertKey>2</concertKey>"));
        assert!(mscx.contains("<concertKey>-1</concertKey>"));
        // Opening 4/4 and mid-stream 3/4 (only in voice 0).
        assert_eq!(mscx.matches("<TimeSig>").count(), 2);
        assert!(mscx.contains("<sigN>3</sigN>"));
        // Voice 2 in measure 2 is a full-measure rest in 3/4.
        assert!(mscx.contains("<durationType>measure</durationType>"));
        assert!(mscx.contains("<duration>3/4</duration>"));

        let data = pack_mscz(&score, &mscx).expect("pack");
        assert!(!has_errors(&validate_mscz(&data)));
    }

    #[test]
    fn ms4_ties_slurs_dynamics_articulations_barlines() {
        let input: ScoreInput = serde_json::from_value(serde_json::json!({
            "title": "Rich Notation",
            "composer": "Test Suite",
            "divisions": 480,
            "time": {"beats": 4, "beat_type": 4},
            "key_fifths": 0,
            "parts": [{
                "name": "Violin",
                "notes": [
                    {"pitch": "B3", "onset": 0.0, "dur": 0.5, "grace": true},
                    {"pitch": "C4", "onset": 0.0, "dur": 1.0, "slur_start": true, "articulations": ["stacc", "accent", "tenuto", "fermata"], "lyric": "la"},
                    {"pitch": "D4", "onset": 1.0, "dur": 1.75, "slur_stop": true},
                    {"pitch": "E4", "onset": 2.75, "dur": 0.25},
                    {"pitch": "F#4", "onset": 3.0, "dur": 2.0}
                ]
            }],
            "measure_attrs": [
                {"measure": 1, "dynamic": "mf", "hairpin": "cresc", "barline_left": "repeat-left", "barline_right": "double"},
                {"measure": 2, "hairpin": "stop", "barline_right": "light-heavy"}
            ]
        }))
        .unwrap();
        let (score, _) = parse_score(&input).unwrap();
        let mut losses = LossTable::default();
        let mscx = to_mscx(&score, &mut losses);

        // Grace note, double-dotted quarter, lyrics, articulations, fermata.
        assert!(mscx.contains("<appoggiatura/>"));
        assert!(mscx.contains("<dots>2</dots>"));
        assert!(mscx.contains("<Lyrics>"));
        assert!(mscx.contains("<text>la</text>"));
        assert!(mscx.contains("<subtype>articStaccatoAbove</subtype>"));
        assert!(mscx.contains("<subtype>articAccentAbove</subtype>"));
        assert!(mscx.contains("<subtype>articTenutoAbove</subtype>"));
        assert!(mscx.contains("<Fermata>"));
        // Dynamic + HairPin spanner.
        assert!(mscx.contains("<Dynamic>"));
        assert!(mscx.contains("<subtype>mf</subtype>"));
        assert!(mscx.contains("<Spanner type=\"HairPin\">"));
        // Slur spanner (start at 0, stop at +1/4).
        assert!(mscx.contains("<Spanner type=\"Slur\">"));
        assert!(mscx.contains("<fractions>1/4</fractions>"));
        assert!(mscx.contains("<fractions>-1/4</fractions>"));
        // Cross-bar tie on F#4 (m1 off 3/4 -> m2 off 0): dm=1, df=-3/4 and dm=-1, df=3/4.
        assert!(mscx.contains("<Spanner type=\"Tie\">"));
        assert!(mscx.contains("<measures>1</measures>"));
        assert!(mscx.contains("<fractions>-3/4</fractions>"));
        assert!(mscx.contains("<measures>-1</measures>"));
        assert!(mscx.contains("<fractions>3/4</fractions>"));
        // Barlines.
        assert!(mscx.contains("<startRepeat/>"));
        assert!(mscx.contains("<subtype>double</subtype>"));
        assert!(mscx.contains("<subtype>end</subtype>"));
        // Zero tie/clef/dynamic/barline/lyric/grace losses in MS4!
        assert_eq!(
            losses.loss_count(),
            0,
            "unexpected losses: {:?}",
            losses.entries
        );
    }

    #[test]
    fn musicxml_dtd_identification_wraps_creator() {
        let (score, _) = simple_score();
        let mut losses = LossTable::default();
        let xml = to_musicxml(&score, &mut losses);
        assert!(
            xml.contains("<identification>\n    <creator type=\"composer\">Example</creator>\n  </identification>"),
            "expected <creator> wrapped in <identification>, got:\n{xml}"
        );
        assert!(!has_errors(&validate_musicxml(&xml)));
    }

    #[test]
    fn musicxml_dtd_grace_before_chord() {
        let input = ScoreInput {
            title: Some("Grace Chord".into()),
            notes: vec![
                NoteInput {
                    grace: true,
                    ..NoteInput::note("E4", 0.0, 0.5)
                },
                NoteInput {
                    grace: true,
                    ..NoteInput::note("G4", 0.0, 0.5)
                },
                NoteInput::note("C4", 0.0, 4.0),
            ],
            ..Default::default()
        };
        let (score, _) = parse_score(&input).unwrap();
        let mut losses = LossTable::default();
        let xml = to_musicxml(&score, &mut losses);
        let chord_pos = xml
            .find("<chord/>")
            .expect("grace chord should emit <chord/>");
        let note_start = xml[..chord_pos]
            .rfind("<note>")
            .expect("chord must be inside <note>");
        let grace_pos = xml[note_start..chord_pos]
            .find("<grace/>")
            .expect("<grace/> must appear before <chord/> inside the grace chord <note>");
        assert!(note_start + grace_pos < chord_pos);
        assert!(!has_errors(&validate_musicxml(&xml)));
    }

    #[test]
    fn musicxml_version_40_emits_and_validates() {
        let (score, _) = simple_score();
        let mut losses = LossTable::default();
        let xml40 = to_musicxml_version(&score, MusicXmlVersion::V40, &mut losses);
        assert!(xml40.contains("MusicXML 4.0 Partwise"));
        assert!(xml40.contains("<score-partwise version=\"4.0\">"));
        let issues = validate_musicxml(&xml40);
        assert!(!has_errors(&issues), "v4.0 validation errors: {issues:?}");

        let xml31 = to_musicxml_version(&score, MusicXmlVersion::V31, &mut losses);
        assert!(xml31.contains("MusicXML 3.1 Partwise"));
        assert!(xml31.contains("<score-partwise version=\"3.1\">"));
        assert!(!has_errors(&validate_musicxml(&xml31)));
    }

    #[test]
    fn pack_mscz_bytes_and_read_mscz_roundtrip() {
        let (score, _) = simple_score();
        let mut losses = LossTable::default();
        let bytes = pack_mscz_bytes(&score, &mut losses).expect("pack_mscz_bytes");
        assert!(!has_errors(&validate_mscz(&bytes)));

        let contents = read_mscz(&bytes).expect("read_mscz");
        assert_eq!(contents.root_mscx_path, "Two-Voice-Prelude.mscx");
        assert!(contents.mscx.contains("<museScore version=\"4.20\">"));
        assert!(contents.mscx.contains("name=\"workTitle\""));
        assert!(contents
            .style_mss
            .as_deref()
            .is_some_and(|s| s.contains("<Style>")));
        assert!(contents
            .entries
            .contains(&"META-INF/container.xml".to_string()));
        assert!(contents
            .entries
            .contains(&"Two-Voice-Prelude.mscx".to_string()));
        assert!(contents.entries.contains(&"score_style.mss".to_string()));

        assert!(read_mscz(b"not a zip").is_err());
    }

    #[test]
    fn validate_musicxml_handles_utf8_note_attrs_unclosed_and_dynamic_divisions() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<score-partwise version="3.1">
  <work><work-title>Frühlingslied — Café &amp; naïve</work-title></work>
  <part-list><score-part id="P1"><part-name>Gesang</part-name></score-part></part-list>
  <part id="P1">
    <measure number="1">
      <attributes><divisions>2</divisions><time><beats>2</beats><beat-type>4</beat-type></time></attributes>
      <note default-x="12.5"><pitch><step>C</step><octave>4</octave></pitch><duration>4</duration><lyric><text>Schön</text></lyric></note>
    </measure>
    <measure number="2">
      <attributes><divisions>4</divisions></attributes>
      <note default-x="10.0"><pitch><step>D</step><octave>4</octave></pitch><duration>8</duration></note>
    </measure>
  </part>
</score-partwise>"#;
        let issues = validate_musicxml(xml);
        assert!(!has_errors(&issues), "unexpected errors: {issues:?}");

        // Unclosed <note> must not panic
        let unclosed = r#"<?xml version="1.0" encoding="UTF-8"?>
<score-partwise version="3.1"><part id="P1"><measure number="1">
<attributes><divisions>1</divisions><time><beats>1</beats><beat-type>4</beat-type></time></attributes>
<note default-x="1"><pitch><step>C</step><octave>4</octave></pitch><duration>1</duration></measure></part></score-partwise>"#;
        let _ = validate_musicxml(unclosed);
    }

    #[test]
    fn microtone_preserves_enharmonic_spelling_when_midi_unchanged() {
        let (step, alter, octave, midi, cents) = parse_pitch("Bb4+10c").unwrap();
        assert_eq!(
            (step, alter, octave, midi, cents),
            ('B', -1, 4, 70, Some(10))
        );
    }

    #[test]
    fn measure_attr_is_last_wins_and_huge_onset_rejected() {
        let (score, _) = ScoreBuilder::new("Last Wins")
            .time(4, 4)
            .measure_attr(
                1,
                MeasureAttr {
                    dynamic: Some("p".to_string()),
                    ..Default::default()
                },
            )
            .measure_attr(
                1,
                MeasureAttr {
                    dynamic: Some("ff".to_string()),
                    ..Default::default()
                },
            )
            .part("Piano", |p| p.note("C4", 0.0, 4.0))
            .build()
            .unwrap();
        assert_eq!(
            measure_attr(&score, 1).and_then(|a| a.dynamic.as_deref()),
            Some("ff")
        );

        let huge = ScoreInput {
            notes: vec![NoteInput::note("C4", 1e9, 1.0)],
            ..Default::default()
        };
        assert!(parse_score(&huge).is_err());
    }

    #[test]
    fn loss_table_summary_counts_and_display() {
        let empty = LossTable::default();
        assert_eq!(empty.summary(), "lossless (0 entries)");
        assert_eq!(format!("{empty}"), "lossless (0 entries)");
        assert!(empty.counts_by_kind().is_empty());

        let input = ScoreInput {
            notes: vec![
                NoteInput::note("A4+30c", 0.0, 1.0),
                NoteInput::note("C4", 0.5, 1.0),
            ],
            ..Default::default()
        };
        let (_, losses) = parse_score(&input).unwrap();
        let counts = losses.counts_by_kind();
        assert_eq!(counts.get("pitch-microtone-rounded"), Some(&1));
        assert_eq!(counts.get("overlap-clamped"), Some(&1));
        let summary = losses.summary();
        assert!(summary.contains("2 loss(es)"));
        assert!(summary.contains("pitch-microtone-rounded=1"));
        assert!(summary.contains("overlap-clamped=1"));
        let rendered = format!("{losses}");
        assert!(rendered.starts_with(&summary));
        assert!(rendered.contains("[loss] pitch-microtone-rounded:"));

        let issue = error("test-code", "bad thing".to_string());
        assert_eq!(format!("{issue}"), "[error] test-code: bad thing");
    }

    #[test]
    fn input_defaults_and_note_helpers() {
        let ts = TimeSig::default();
        assert_eq!(
            ts,
            TimeSig {
                beats: 4,
                beat_type: 4
            }
        );
        let p = PartInput::default();
        assert_eq!(p.name, "Part 1");
        let n = NoteInput::note("D4", 1.0, 0.5).with_voice(2);
        assert_eq!(n.pitch.as_deref(), Some("D4"));
        assert_eq!(n.voice, 2);
        assert!(!n.rest);
        let r = NoteInput::rest(2.0, 1.0);
        assert!(r.rest);
        let s = ScoreInput {
            notes: vec![NoteInput::note("C4", 0.0, 4.0)],
            ..Default::default()
        };
        assert_eq!(s.divisions, SCORE_DIVISIONS);
        assert!(parse_score(&s).is_ok());
    }
}
