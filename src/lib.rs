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
    /// Explicit rational tuplet `(actual_notes, normal_notes)` (e.g. `(3, 2)`,
    /// `(6, 4)`, `(11, 8)`). When present, MusicXML rendering emits exact
    /// `<time-modification>` and written `<type>` instead of guessing from ticks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tuplet: Option<(u32, u32)>,
    /// Cue-sized note (`<cue/>`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cue: bool,
    /// Relative cross-staff shift (`-1` = `xstaff-up`, `+1` = `xstaff-down`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xstaff: Option<i8>,
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
            tuplet: None,
            cue: false,
            xstaff: None,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clef_sign: Option<char>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clef_line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clef_octave_change: Option<i32>,
    /// Optional per-part initial key signature in fifths (overrides `ScoreInput.key_fifths`
    /// for transposing instruments such as Bb clarinet + piano).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_fifths: Option<i32>,
    /// Part/staff-scoped measure overrides (mid-piece clefs, dynamics, hairpins).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub measure_attrs: Vec<MeasureAttr>,
}

impl Default for PartInput {
    fn default() -> Self {
        Self {
            name: default_part_name(),
            notes: Vec::new(),
            clef_sign: None,
            clef_line: None,
            clef_octave_change: None,
            key_fifths: None,
            measure_attrs: Vec::new(),
        }
    }
}

fn default_part_name() -> String {
    "Part 1".to_string()
}

/// Cursor-anchored event within a measure (`bekern+v2` inline dynamics,
/// hairpins, ottava shifts, and mid-bar clef/key/time changes).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimedEvent {
    /// Beat offset within the measure (`0.0` = beat 0 / measure start).
    pub onset_beats: f64,
    /// Event kind: `"dynamic"`, `"hairpin"`, `"ottava"`, `"clef"`, `"key"`, `"time"`.
    pub kind: String,
    /// Event payload (e.g. `"f"`, `"cresc"`, `"dim"`, `"stop"`, `"8va"`, `"8vb"`,
    /// `"G-2"`, `"F-4-8vb"`, `"+2"`, `"3-4"`).
    pub value: String,
}

/// Measure-scoped notation state changes (bekern+ mid-stream headers,
/// dynamics, hairpins, barlines, voltas, and cursor-anchored inline events).
/// `measure` is 1-based.
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
    /// Additional dynamics in order when a measure contains more than one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dynamics: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hairpin: Option<String>,
    /// Additional hairpin events in order when a measure contains more than one
    /// (e.g. `cresc` followed by `stop` in the same measure).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hairpins: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub barline_left: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub barline_right: Option<String>,
    /// Volta / ending bracket number at measure start (e.g. `"1"`, `"2"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volta: Option<String>,
    /// Cursor-anchored inline events (dynamics, hairpins, ottavas, mid-bar
    /// clef/key/time changes) placed at their exact beat offset within the measure.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub timed_events: Vec<TimedEvent>,
}

impl MeasureAttr {
    /// Append a dynamic mark without overwriting an earlier dynamic in the same measure.
    pub fn push_dynamic(&mut self, dyn_name: impl Into<String>) {
        let d = dyn_name.into();
        if self.dynamic.is_none() {
            self.dynamic = Some(d);
        } else {
            self.dynamics.push(d);
        }
    }

    /// All dynamic marks in this measure in order (`dynamic` followed by `dynamics`).
    pub fn all_dynamics(&self) -> Vec<&str> {
        let mut out = Vec::with_capacity(usize::from(self.dynamic.is_some()) + self.dynamics.len());
        if let Some(d) = self.dynamic.as_deref() {
            out.push(d);
        }
        out.extend(self.dynamics.iter().map(String::as_str));
        out
    }

    /// Append a hairpin event without overwriting an earlier hairpin in the same measure.
    pub fn push_hairpin(&mut self, hp: impl Into<String>) {
        let h = hp.into();
        if self.hairpin.is_none() {
            self.hairpin = Some(h);
        } else {
            self.hairpins.push(h);
        }
    }

    /// All hairpin events in this measure in order (`hairpin` followed by `hairpins`).
    pub fn all_hairpins(&self) -> Vec<&str> {
        let mut out = Vec::with_capacity(usize::from(self.hairpin.is_some()) + self.hairpins.len());
        if let Some(h) = self.hairpin.as_deref() {
            out.push(h);
        }
        out.extend(self.hairpins.iter().map(String::as_str));
        out
    }
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
    pub tuplet: Option<(u32, u32)>,
    pub cue: bool,
    pub xstaff: Option<i8>,
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
    pub clef_sign: Option<char>,
    pub clef_line: Option<u32>,
    pub clef_octave_change: Option<i32>,
    pub key_fifths: Option<i32>,
    pub measure_attrs: Vec<MeasureAttr>,
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
    const BASES: [(&str, i64); 8] = [
        ("breve", -8),
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

/// Try to express a non-dyadic tick duration as a standard note type (+ dots)
/// scaled by a common tuplet ratio `(actual_notes, normal_notes)`, i.e.
/// `dur_ticks ~= base_ticks * normal_notes / actual_notes` (within +/- 1 tick).
///
/// Returns `Some((type_name, dots, base_ticks, actual_notes, normal_notes))`
/// only when `quantize_duration(dur_ticks, divisions)` is not already exact.
fn tuplet_duration(
    dur_ticks: i64,
    divisions: u32,
) -> Option<(&'static str, u8, i64, u32, u32)> {
    if dur_ticks <= 0 {
        return None;
    }
    let (_, _, exact_shown) = quantize_duration(dur_ticks, divisions);
    if exact_shown == dur_ticks {
        return None;
    }
    const RATIOS: [(u32, u32); 8] = [
        (3, 2),
        (5, 4),
        (5, 2),
        (7, 4),
        (7, 8),
        (9, 8),
        (3, 4),
        (2, 3),
    ];
    for (actual, normal) in RATIOS {
        let written_est = (dur_ticks * actual as i64 + normal as i64 / 2) / normal as i64;
        let (name, dots, base_ticks) = quantize_duration(written_est, divisions);
        let expected = (base_ticks * normal as i64 + actual as i64 / 2) / actual as i64;
        if (expected - dur_ticks).abs() <= 1 {
            return Some((name, dots, base_ticks, actual, normal));
        }
    }
    None
}

/// Undotted base duration name and tick count for a MuseScore `<Tuplet><baseNote>`.
fn undotted_base_note(base_ticks: i64, dots: u8, divisions: u32) -> (&'static str, i64) {
    if dots == 0 {
        let (name, _, val) = quantize_duration(base_ticks, divisions);
        return (name, val);
    }
    // Strip dots: base_ticks = undotted * (2^(dots+1) - 1) / 2^dots
    let num = (1i64 << (dots + 1)) - 1;
    let den = 1i64 << dots;
    let undotted = (base_ticks * den + num / 2) / num;
    let (name, _, val) = quantize_duration(undotted, divisions);
    (name, val)
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

    type RawPart<'a> = (
        String,
        &'a [NoteInput],
        Option<char>,
        Option<u32>,
        Option<i32>,
        Option<i32>,
        &'a [MeasureAttr],
    );
    let raw_parts: Vec<RawPart<'_>> = if !input.parts.is_empty() {
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
                    p.clef_sign,
                    p.clef_line,
                    p.clef_octave_change,
                    p.key_fifths,
                    p.measure_attrs.as_slice(),
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
            None,
            None,
            None,
            None,
            &[],
        )]
    };
    if raw_parts.iter().all(|(_, n, _, _, _, _, _)| n.is_empty()) {
        return Err("score has no notes".to_string());
    }

    let mut losses = LossTable::default();
    let mut parts = Vec::new();
    for (pname, notes, clef_sign, clef_line, clef_octave_change, part_key_fifths, part_attrs) in
        &raw_parts
    {
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
                        tuplet: n.tuplet,
                        cue: n.cue,
                        xstaff: n.xstaff,
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
                    tuplet: n.tuplet,
                    cue: n.cue,
                    xstaff: n.xstaff,
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
        let mut p_measure_attrs = part_attrs.to_vec();
        if !p_measure_attrs.is_sorted_by_key(|a| a.measure) {
            p_measure_attrs.sort_by_key(|a| a.measure);
        }
        parts.push(Part {
            name: pname.clone(),
            groups,
            voices,
            end_ticks,
            clef_sign: *clef_sign,
            clef_line: *clef_line,
            clef_octave_change: *clef_octave_change,
            key_fifths: *part_key_fifths,
            measure_attrs: p_measure_attrs,
        });
    }

    let mut measure_attrs = input.measure_attrs.clone();
    if !measure_attrs.is_sorted_by_key(|a| a.measure) {
        measure_attrs.sort_by_key(|a| a.measure);
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
            measure_attrs,
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
    /// True for implicit gap padding inserted in secondary polyphonic voices
    /// (`vi > 0`): emitted as hidden `<forward>` in MusicXML instead of a
    /// visible `<rest>` so sparse inner voices do not inflate rest counts.
    forward: bool,
}

/// Split a part's chord groups into per-measure, per-voice segments.
/// Groups crossing a barline are split with ties (lossless). Gaps are
/// filled with rests in the primary voice and hidden `<forward>` skips
/// in secondary polyphonic voices; every emitted voice sums to `measure_ticks`.
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
        for (vi, v) in part.voices.iter().enumerate() {
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
                        forward: false,
                    });
                }
            }
            // Sparse secondary polyphonic voices with no events in this measure
            // are omitted rather than padded with spurious full-bar visible rests.
            if vi > 0 && segs.is_empty() {
                continue;
            }
            segs.sort_by_key(|s| (s.note.start_ticks.max(m0), s.chord));
            // Fill gaps with rests (primary voice) or hidden `<forward>` skips
            // (secondary polyphonic voices) so every emitted voice sums to mlen.
            let is_secondary = vi > 0;
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
                            tuplet: None,
                            cue: false,
                            xstaff: None,
                        },
                        chord: false,
                        dur_ticks: ss - cursor,
                        tie_in: false,
                        tie_out: false,
                        forward: is_secondary,
                    });
                    if !is_secondary {
                        rest_inserted = true;
                    }
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
                        tuplet: None,
                        cue: false,
                        xstaff: None,
                    },
                    chord: false,
                    dur_ticks: m1 - cursor,
                    tie_in: false,
                    tie_out: false,
                    forward: is_secondary,
                });
                if !is_secondary {
                    rest_inserted = true;
                }
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
    let has_art = seg.note.articulations.iter().any(|a| {
        matches!(
            a.as_str(),
            "stacc" | "accent" | "tenuto" | "art-staccatissimo" | "art-soft-accent"
        )
    });
    let has_orn = seg.note.articulations.iter().any(|a| {
        matches!(
            a.as_str(),
            "art-trill" | "art-tremolo" | "art-mordent" | "art-turn"
        )
    });
    let has_tech = seg
        .note
        .articulations
        .iter()
        .any(|a| matches!(a.as_str(), "art-up-bow" | "art-down-bow"));
    let fermata = seg.note.articulations.iter().any(|a| a == "fermata");
    let arpeggiate = seg
        .note
        .articulations
        .iter()
        .any(|a| a == "art-arpeggiate");
    if !tie_start
        && !tie_stop
        && !seg.note.slur_start
        && !seg.note.slur_stop
        && !has_art
        && !has_orn
        && !has_tech
        && !fermata
        && !arpeggiate
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
                "art-staccatissimo" => out.push_str("<staccatissimo/>"),
                "art-soft-accent" => out.push_str("<soft-accent/>"),
                _ => {}
            }
        }
        out.push_str("</articulations>");
    }
    if has_orn {
        out.push_str("<ornaments>");
        for a in &seg.note.articulations {
            match a.as_str() {
                "art-trill" => out.push_str("<trill-mark/>"),
                "art-tremolo" => out.push_str("<tremolo>3</tremolo>"),
                "art-mordent" => out.push_str("<inverted-mordent/>"),
                "art-turn" => out.push_str("<turn/>"),
                _ => {}
            }
        }
        out.push_str("</ornaments>");
    }
    if has_tech {
        out.push_str("<technical>");
        for a in &seg.note.articulations {
            match a.as_str() {
                "art-up-bow" => out.push_str("<up-bow/>"),
                "art-down-bow" => out.push_str("<down-bow/>"),
                _ => {}
            }
        }
        out.push_str("</technical>");
    }
    if fermata {
        out.push_str("<fermata/>");
    }
    if arpeggiate {
        out.push_str("<arpeggiate/>");
    }
    out.push_str("</notations>\n");
}

fn push_forward_xml(out: &mut String, dur_ticks: i64, voice_num: u32, staff_num: Option<u32>) {
    let _ = write!(
        out,
        "      <forward><duration>{dur_ticks}</duration><voice>{voice_num}</voice>"
    );
    if let Some(st) = staff_num {
        let _ = write!(out, "<staff>{st}</staff>");
    }
    out.push_str("</forward>\n");
}

fn push_note_xml(
    out: &mut String,
    seg: &Segment,
    divisions: u32,
    losses: &mut LossTable,
    part_name: &str,
    voice_override: Option<u32>,
    staff_num: Option<u32>,
) {
    let voice_num = voice_override.unwrap_or(seg.note.voice);
    let effective_staff = staff_num.map(|st| match seg.note.xstaff {
        Some(delta) => (st as i32 + delta as i32).clamp(1, 2) as u32,
        None => st,
    });
    // Grace notes occupy no time: `<grace/>` plus the engraved `<type>`
    // for display, and no `<duration>` (which is also what the scorer
    // reads, so grace round-trips as zero-duration events).
    if seg.note.grace {
        out.push_str("      <note>\n");
        out.push_str("        <grace/>\n");
        if seg.note.cue {
            out.push_str("        <cue/>\n");
        }
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
        let _ = writeln!(out, "        <voice>{voice_num}</voice>");
        let _ = writeln!(out, "        <type>{typename}</type>");
        for _ in 0..dots {
            out.push_str("        <dot/>\n");
        }
        if !seg.note.rest {
            if let Some(acc) = accidental_name(seg.note.alter) {
                let _ = writeln!(out, "        <accidental>{acc}</accidental>");
            }
        }
        if let Some(st) = effective_staff {
            let _ = writeln!(out, "        <staff>{st}</staff>");
        }
        write_notations_xml(out, seg);
        out.push_str("      </note>\n");
        return;
    }
    // `<duration>` carries the exact tick count (timing is lossless).
    // Prefer explicit rational `seg.note.tuplet` when present (exact
    // `<time-modification>` for any ratio such as 6:4, 11:8, 13:8); otherwise
    // fall back to the tick-based `tuplet_duration` guesser, then nearest dyadic.
    let tuplet_mod = if let Some((actual, normal)) = seg.note.tuplet {
        if actual > 0 && normal > 0 {
            let written_est =
                (seg.dur_ticks * actual as i64 + normal as i64 / 2) / normal as i64;
            let (name, dots, base_ticks) = quantize_duration(written_est, divisions);
            Some((name, dots, base_ticks, actual, normal))
        } else {
            tuplet_duration(seg.dur_ticks, divisions)
        }
    } else {
        tuplet_duration(seg.dur_ticks, divisions)
    };
    let (typename, dots) = if let Some((tn, td, _, _, _)) = tuplet_mod {
        (tn, td)
    } else {
        let (tn, td, shown) = quantize_duration(seg.dur_ticks, divisions);
        if shown != seg.dur_ticks {
            losses.push(
                "loss",
                "note-type-approximated",
                format!(
                    "{} ticks shown as {tn}{} ({shown} ticks)",
                    seg.dur_ticks,
                    ".".repeat(td as usize),
                ),
                Some(part_name.to_string()),
                None,
            );
        }
        (tn, td)
    };
    let dur = seg.dur_ticks;
    out.push_str("      <note>\n");
    if seg.note.cue {
        out.push_str("        <cue/>\n");
    }
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
    let _ = writeln!(out, "        <voice>{voice_num}</voice>");
    let _ = writeln!(out, "        <type>{typename}</type>");
    for _ in 0..dots {
        out.push_str("        <dot/>\n");
    }
    if !seg.note.rest {
        if let Some(acc) = accidental_name(seg.note.alter) {
            let _ = writeln!(out, "        <accidental>{acc}</accidental>");
        }
    }
    if let Some((_, _, _, actual, normal)) = tuplet_mod {
        let _ = writeln!(
            out,
            "        <time-modification><actual-notes>{actual}</actual-notes><normal-notes>{normal}</normal-notes></time-modification>"
        );
    }
    if let Some(st) = effective_staff {
        let _ = writeln!(out, "        <staff>{st}</staff>");
    }
    if !seg.note.rest {
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
fn write_clef_xml(
    out: &mut String,
    sign: char,
    line: u32,
    octave_change: Option<i32>,
    number: Option<u32>,
) {
    if let Some(n) = number {
        let _ = write!(out, "        <clef number=\"{n}\"><sign>");
    } else {
        out.push_str("        <clef><sign>");
    }
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

/// Measure-scoped override for one 1-based measure number in a sorted slice (last-wins).
fn measure_attr_in(attrs: &[MeasureAttr], measure: u32) -> Option<&MeasureAttr> {
    if attrs.is_empty() {
        return None;
    }
    let idx = attrs.partition_point(|a| a.measure <= measure);
    if idx > 0 && attrs[idx - 1].measure == measure {
        Some(&attrs[idx - 1])
    } else {
        None
    }
}

/// Measure-scoped override for one 1-based measure number, if any (last-wins).
fn measure_attr(score: &Score, measure: u32) -> Option<&MeasureAttr> {
    measure_attr_in(&score.measure_attrs, measure)
}

/// Part-scoped measure override for one 1-based measure number, if any (last-wins).
fn part_measure_attr(part: &Part, measure: u32) -> Option<&MeasureAttr> {
    measure_attr_in(&part.measure_attrs, measure)
}

/// True when the last two parts form a standard piano grand-staff pair
/// (`clef-G-2` followed by `clef-F-4`, neither octave-transposing) so MusicXML
/// emits them as one `<part>` with `<staves>2</staves>` without breaking
/// string quartets `(G2, G2, C3, F4)`.
fn has_trailing_grand_staff(parts: &[Part]) -> bool {
    if parts.len() < 2 {
        return false;
    }
    let hi = &parts[parts.len() - 2];
    let lo = &parts[parts.len() - 1];
    hi.clef_sign == Some('G')
        && hi.clef_line.unwrap_or(2) == 2
        && hi.clef_octave_change.is_none()
        && lo.clef_sign == Some('F')
        && lo.clef_line.unwrap_or(4) == 4
        && lo.clef_octave_change.is_none()
}

/// Write `<direction>` carrying one dynamic mark, with optional tick `<offset>`.
fn write_dyn_direction_xml(out: &mut String, dynamic: &str, staff_num: Option<u32>) {
    write_dyn_direction_xml_offset(out, dynamic, staff_num, None);
}

fn write_dyn_direction_xml_offset(
    out: &mut String,
    dynamic: &str,
    staff_num: Option<u32>,
    offset_ticks: Option<i64>,
) {
    out.push_str("      <direction placement=\"below\"><direction-type><dynamics><");
    out.push_str(dynamic);
    out.push_str("/></dynamics></direction-type>");
    if let Some(off) = offset_ticks {
        if off != 0 {
            let _ = write!(out, "<offset>{off}</offset>");
        }
    }
    if let Some(st) = staff_num {
        let _ = write!(out, "<staff>{st}</staff>");
    }
    out.push_str("</direction>\n");
}

/// Write `<direction>` carrying one hairpin wedge event, with optional tick `<offset>`.
fn write_hairpin_direction_xml(out: &mut String, hairpin: &str, staff_num: Option<u32>) {
    write_hairpin_direction_xml_offset(out, hairpin, staff_num, None);
}

fn write_hairpin_direction_xml_offset(
    out: &mut String,
    hairpin: &str,
    staff_num: Option<u32>,
    offset_ticks: Option<i64>,
) {
    let wedge = match hairpin {
        "cresc" => "crescendo",
        "dim" => "diminuendo",
        _ => "stop",
    };
    let _ = write!(
        out,
        "      <direction placement=\"below\"><direction-type><wedge type=\"{wedge}\"/></direction-type>"
    );
    if let Some(off) = offset_ticks {
        if off != 0 {
            let _ = write!(out, "<offset>{off}</offset>");
        }
    }
    if let Some(st) = staff_num {
        let _ = write!(out, "<staff>{st}</staff>");
    }
    out.push_str("</direction>\n");
}

/// Write `<direction>` carrying an `<octave-shift>` (`8va` = down, `8vb` = up, `stop`).
fn write_ottava_direction_xml(
    out: &mut String,
    ottava: &str,
    staff_num: Option<u32>,
    offset_ticks: Option<i64>,
) {
    let (otype, placement) = match ottava {
        "8va" => ("down", "above"),
        "8vb" => ("up", "below"),
        _ => ("stop", "above"),
    };
    let _ = write!(
        out,
        "      <direction placement=\"{placement}\"><direction-type><octave-shift type=\"{otype}\" size=\"8\"/></direction-type>"
    );
    if let Some(off) = offset_ticks {
        if off != 0 {
            let _ = write!(out, "<offset>{off}</offset>");
        }
    }
    if let Some(st) = staff_num {
        let _ = write!(out, "<staff>{st}</staff>");
    }
    out.push_str("</direction>\n");
}

/// Write a mid-bar `<attributes>` element for a cursor-anchored clef/key/time change.
fn write_midbar_attr_xml(out: &mut String, kind: &str, value: &str, staff_num: Option<u32>) {
    match kind {
        "clef" => {
            let mut parts = value.split('-');
            let sign = parts.next().and_then(|s| s.chars().next()).unwrap_or('G');
            let line: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(2);
            let oct = match parts.next() {
                Some("8vb") => Some(-1),
                Some("8va") => Some(1),
                _ => None,
            };
            out.push_str("      <attributes>\n");
            write_clef_xml(out, sign, line, oct, staff_num);
            out.push_str("      </attributes>\n");
        }
        "key" => {
            if let Ok(k) = value.parse::<i32>() {
                out.push_str("      <attributes>\n");
                let _ = writeln!(out, "        <key><fifths>{k}</fifths></key>");
                out.push_str("      </attributes>\n");
            }
        }
        "time" => {
            let mut parts = value.split('-');
            if let (Some(b), Some(t)) = (
                parts.next().and_then(|s| s.parse::<u32>().ok()),
                parts.next().and_then(|s| s.parse::<u32>().ok()),
            ) {
                out.push_str("      <attributes>\n");
                let _ = writeln!(
                    out,
                    "        <time><beats>{b}</beats><beat-type>{t}</beat-type></time>"
                );
                out.push_str("      </attributes>\n");
            }
        }
        _ => {}
    }
}

/// Write one [`TimedEvent`] at the current cursor (with optional `<offset>` for directions).
fn write_timed_event_xml(
    out: &mut String,
    ev: &TimedEvent,
    staff_num: Option<u32>,
    offset_ticks: Option<i64>,
) {
    match ev.kind.as_str() {
        "dynamic" => write_dyn_direction_xml_offset(out, &ev.value, staff_num, offset_ticks),
        "hairpin" => write_hairpin_direction_xml_offset(out, &ev.value, staff_num, offset_ticks),
        "ottava" => write_ottava_direction_xml(out, &ev.value, staff_num, offset_ticks),
        "clef" | "key" | "time" => write_midbar_attr_xml(out, &ev.kind, &ev.value, staff_num),
        _ => {}
    }
}

/// Emit a voice's segments while interleaving cursor-anchored `timed_events` at their
/// exact tick positions (using `<offset>` when an event falls strictly inside a note).
#[allow(clippy::too_many_arguments)]
fn push_voice_with_timed_events(
    out: &mut String,
    segs: &[Segment],
    m0: i64,
    divisions: u32,
    losses: &mut LossTable,
    part_name: &str,
    staff_num: Option<u32>,
    timed: &[(i64, &TimedEvent)],
) {
    let mut ti = 0usize;
    let mut cur_off = 0i64;
    for s in segs {
        let seg_off = s.note.start_ticks.max(m0) - m0;
        if !s.chord {
            // Flush events at or before this note's onset right at `cur_off`.
            while ti < timed.len() && timed[ti].0 <= seg_off {
                let delta = timed[ti].0 - cur_off;
                let off = if delta > 0 { Some(delta) } else { None };
                write_timed_event_xml(out, timed[ti].1, staff_num, off);
                ti += 1;
            }
            // Also flush direction events falling strictly inside this note's span
            // using `<offset>` relative to `cur_off`, so onset stays exact even if
            // voice 1 holds a longer note than the voice carrying the dynamic.
            let seg_end = seg_off + if s.note.grace { 0 } else { s.dur_ticks };
            while ti < timed.len()
                && timed[ti].0 < seg_end
                && matches!(
                    timed[ti].1.kind.as_str(),
                    "dynamic" | "hairpin" | "ottava"
                )
            {
                let delta = timed[ti].0 - cur_off;
                let off = if delta > 0 { Some(delta) } else { None };
                write_timed_event_xml(out, timed[ti].1, staff_num, off);
                ti += 1;
            }
        }
        push_note_xml(out, s, divisions, losses, part_name, None, staff_num);
        if !s.chord && !s.note.grace {
            cur_off = cur_off.max(seg_off + s.dur_ticks);
        }
    }
    while ti < timed.len() {
        write_timed_event_xml(out, timed[ti].1, staff_num, None);
        ti += 1;
    }
}

/// Write a left `<barline>` carrying an optional forward repeat and/or volta ending.
fn write_left_barline_xml(out: &mut String, barline_left: Option<&str>, volta: Option<&str>) {
    if barline_left.is_none() && volta.is_none() {
        return;
    }
    out.push_str("      <barline location=\"left\">");
    if barline_left == Some("repeat-left") {
        out.push_str("<bar-style>heavy-light</bar-style><repeat direction=\"forward\"/>");
    }
    if let Some(v) = volta {
        let _ = write!(out, "<ending number=\"{v}\" type=\"start\"/>");
    }
    out.push_str("</barline>\n");
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

/// Render a clean empty 0-measure MusicXML 3.1 `<score-partwise>` document
/// (used when a page has 0 pitched notes and abstains, e.g. blank-staff or
/// text-only negative controls).
pub fn to_empty_musicxml(title: &str) -> String {
    let mut out = String::with_capacity(512);
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str("<!DOCTYPE score-partwise PUBLIC \"-//Recordare//DTD MusicXML 3.1 Partwise//EN\" \"http://www.musicxml.org/dtds/partwise.dtd\">\n");
    out.push_str("<score-partwise version=\"3.1\">\n");
    out.push_str("  <work><work-title>");
    write_escaped_xml(&mut out, title);
    out.push_str("</work-title></work>\n");
    out.push_str("  <part-list>\n");
    out.push_str("    <score-part id=\"P1\"><part-name>Part 1</part-name></score-part>\n");
    out.push_str("  </part-list>\n");
    out.push_str("  <part id=\"P1\">\n");
    out.push_str("  </part>\n");
    out.push_str("</score-partwise>\n");
    out
}

/// Render a clean empty 0-measure MuseScore 4 `.mscx` document for abstained
/// zero-note pages.
pub fn to_empty_mscx(title: &str) -> String {
    let mut out = String::with_capacity(512);
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str("<museScore version=\"4.20\">\n");
    out.push_str("  <programVersion>4.2.0</programVersion>\n");
    out.push_str("  <Score>\n");
    let _ = writeln!(out, "    <Division>{SCORE_DIVISIONS}</Division>");
    out.push_str("    <metaTag name=\"workTitle\">");
    write_escaped_xml(&mut out, title);
    out.push_str("</metaTag>\n");
    out.push_str("    <Part id=\"1\">\n");
    out.push_str("      <Staff>\n");
    out.push_str("        <StaffType group=\"pitched\">\n");
    out.push_str("          <name>stdNormal</name>\n");
    out.push_str("          </StaffType>\n");
    out.push_str("        </Staff>\n");
    out.push_str("      <trackName>Part 1</trackName>\n");
    out.push_str("      <Instrument>\n");
    out.push_str("        <longName>Part 1</longName>\n");
    out.push_str("        <trackName>Part 1</trackName>\n");
    out.push_str("        </Instrument>\n");
    out.push_str("      </Part>\n");
    out.push_str("    <Staff id=\"1\">\n");
    out.push_str("      </Staff>\n");
    out.push_str("    </Score>\n");
    out.push_str("  </museScore>\n");
    out
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

    let grand_staff = has_trailing_grand_staff(&score.parts);
    let single_count = if grand_staff {
        score.parts.len() - 2
    } else {
        score.parts.len()
    };

    out.push_str("  <part-list>\n");
    for (i, part) in score.parts.iter().take(single_count).enumerate() {
        let _ = write!(out, "    <score-part id=\"P{}\"><part-name>", i + 1);
        write_escaped_xml(&mut out, &part.name);
        out.push_str("</part-name></score-part>\n");
    }
    if grand_staff {
        let p_id = single_count + 1;
        let hi_name = &score.parts[single_count].name;
        let gs_name = if hi_name.starts_with("Staff ") {
            "Piano"
        } else {
            hi_name.as_str()
        };
        let _ = write!(out, "    <score-part id=\"P{p_id}\"><part-name>");
        write_escaped_xml(&mut out, gs_name);
        out.push_str("</part-name></score-part>\n");
    }
    out.push_str("  </part-list>\n");

    let windows = score.measure_windows();
    for (i, part) in score.parts.iter().take(single_count).enumerate() {
        let _ = writeln!(out, "  <part id=\"P{}\">", i + 1);
        let layout = layout_part(score, part, losses);
        for (m, voices) in layout.iter().enumerate() {
            let mlen = windows
                .get(m)
                .map(|&(a, b)| b - a)
                .unwrap_or_else(|| score.measure_ticks());
            let _ = writeln!(out, "    <measure number=\"{}\">", m + 1);
            let m1 = m as u32 + 1;
            let s_attrs = measure_attr(score, m1);
            let p_attrs = part_measure_attr(part, m1);
            if m == 0 {
                let (clef_s, clef_l, clef_oct) = if let Some(s) = part.clef_sign {
                    (s, part.clef_line.unwrap_or(2), part.clef_octave_change)
                } else {
                    match p_attrs.or(s_attrs) {
                        Some(a) => (
                            a.clef_sign.unwrap_or('G'),
                            a.clef_line.unwrap_or(2),
                            a.clef_octave_change,
                        ),
                        None => ('G', 2, None),
                    }
                };
                let key = p_attrs
                    .and_then(|a| a.key_fifths)
                    .or(part.key_fifths)
                    .or_else(|| s_attrs.and_then(|a| a.key_fifths))
                    .unwrap_or(score.key_fifths);
                let (beats, beat_type) = match p_attrs
                    .and_then(|a| a.time)
                    .or_else(|| s_attrs.and_then(|a| a.time))
                {
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
                write_clef_xml(&mut out, clef_s, clef_l, clef_oct, None);
                out.push_str("      </attributes>\n");
            } else {
                let p_has_clef = p_attrs.is_some_and(|a| {
                    a.clef_sign.is_some() || a.clef_line.is_some() || a.clef_octave_change.is_some()
                });
                let s_has_clef = s_attrs.is_some_and(|a| {
                    a.clef_sign.is_some() || a.clef_line.is_some() || a.clef_octave_change.is_some()
                });
                let clef_src = if p_has_clef {
                    p_attrs
                } else if s_has_clef {
                    s_attrs
                } else {
                    None
                };
                let key_change = p_attrs
                    .and_then(|a| a.key_fifths)
                    .or_else(|| s_attrs.and_then(|a| a.key_fifths));
                let time_change = p_attrs
                    .and_then(|a| a.time)
                    .or_else(|| s_attrs.and_then(|a| a.time));
                if clef_src.is_some() || key_change.is_some() || time_change.is_some() {
                    out.push_str("      <attributes>\n");
                    if let Some(k) = key_change {
                        let _ = writeln!(out, "        <key><fifths>{k}</fifths></key>");
                    }
                    if let Some(t) = time_change {
                        let _ = writeln!(
                            out,
                            "        <time><beats>{}</beats><beat-type>{}</beat-type></time>",
                            t.beats, t.beat_type
                        );
                    }
                    if let Some(a) = clef_src {
                        write_clef_xml(
                            &mut out,
                            a.clef_sign.unwrap_or('G'),
                            a.clef_line.unwrap_or(2),
                            a.clef_octave_change,
                            None,
                        );
                    }
                    out.push_str("      </attributes>\n");
                }
            }
            let barline_left = p_attrs
                .and_then(|a| a.barline_left.as_deref())
                .or_else(|| s_attrs.and_then(|a| a.barline_left.as_deref()));
            let volta = p_attrs
                .and_then(|a| a.volta.as_deref())
                .or_else(|| s_attrs.and_then(|a| a.volta.as_deref()));
            write_left_barline_xml(&mut out, barline_left, volta);
            let mut timed: Vec<(i64, &TimedEvent)> = Vec::new();
            let mut s_has_timed_dyn = false;
            let mut s_has_timed_hp = false;
            let mut p_has_timed_dyn = false;
            let mut p_has_timed_hp = false;
            if let Some(a) = s_attrs {
                for te in &a.timed_events {
                    if te.kind == "dynamic" {
                        s_has_timed_dyn = true;
                    } else if te.kind == "hairpin" {
                        s_has_timed_hp = true;
                    }
                    let tick = (te.onset_beats * score.divisions as f64).round() as i64;
                    timed.push((tick.max(0), te));
                }
            }
            if let Some(a) = p_attrs {
                for te in &a.timed_events {
                    if te.kind == "dynamic" {
                        p_has_timed_dyn = true;
                    } else if te.kind == "hairpin" {
                        p_has_timed_hp = true;
                    }
                    let tick = (te.onset_beats * score.divisions as f64).round() as i64;
                    timed.push((tick.max(0), te));
                }
            }
            timed.sort_by_key(|(tick, _)| *tick);
            if let Some(a) = s_attrs {
                if !s_has_timed_dyn {
                    for d in a.all_dynamics() {
                        write_dyn_direction_xml(&mut out, d, None);
                    }
                }
                if !s_has_timed_hp {
                    for h in a.all_hairpins() {
                        write_hairpin_direction_xml(&mut out, h, None);
                    }
                }
            }
            if let Some(a) = p_attrs {
                if !p_has_timed_dyn {
                    for d in a.all_dynamics() {
                        write_dyn_direction_xml(&mut out, d, None);
                    }
                }
                if !p_has_timed_hp {
                    for h in a.all_hairpins() {
                        write_hairpin_direction_xml(&mut out, h, None);
                    }
                }
            }
            let m0 = windows.get(m).map(|&(a, _)| a).unwrap_or(0);
            if voices.is_empty() {
                for &(_, te) in &timed {
                    write_timed_event_xml(&mut out, te, None, None);
                }
            } else {
                for (vi, (_voice, segs)) in voices.iter().enumerate() {
                    if vi > 0 {
                        let _ = writeln!(out, "      <backup><duration>{mlen}</duration></backup>");
                        for s in segs {
                            if s.forward {
                                push_forward_xml(&mut out, s.dur_ticks, s.note.voice, None);
                            } else {
                                push_note_xml(
                                    &mut out,
                                    s,
                                    score.divisions,
                                    losses,
                                    &part.name,
                                    None,
                                    None,
                                );
                            }
                        }
                    } else {
                        push_voice_with_timed_events(
                            &mut out,
                            segs,
                            m0,
                            score.divisions,
                            losses,
                            &part.name,
                            None,
                            &timed,
                        );
                    }
                }
            }
            if let Some(b) = p_attrs
                .and_then(|a| a.barline_right.as_deref())
                .or_else(|| s_attrs.and_then(|a| a.barline_right.as_deref()))
            {
                write_barline_xml(&mut out, true, b);
            }
            out.push_str("    </measure>\n");
        }
        out.push_str("  </part>\n");
    }

    if grand_staff {
        let p_id = single_count + 1;
        let part_hi = &score.parts[single_count];
        let part_lo = &score.parts[single_count + 1];
        let layout_hi = layout_part(score, part_hi, losses);
        let layout_lo = layout_part(score, part_lo, losses);
        let n_measures = layout_hi.len().max(layout_lo.len());

        let _ = writeln!(out, "  <part id=\"P{p_id}\">");
        for m in 0..n_measures {
            let mlen = windows
                .get(m)
                .map(|&(a, b)| b - a)
                .unwrap_or_else(|| score.measure_ticks());
            let _ = writeln!(out, "    <measure number=\"{}\">", m + 1);
            let m1 = m as u32 + 1;
            let s_attrs = measure_attr(score, m1);
            let p1_attrs = part_measure_attr(part_hi, m1);
            let p2_attrs = part_measure_attr(part_lo, m1);
            if m == 0 {
                let key = p1_attrs
                    .and_then(|a| a.key_fifths)
                    .or(part_hi.key_fifths)
                    .or(part_lo.key_fifths)
                    .or_else(|| s_attrs.and_then(|a| a.key_fifths))
                    .unwrap_or(score.key_fifths);
                let (beats, beat_type) = match s_attrs.and_then(|a| a.time) {
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
                out.push_str("        <staves>2</staves>\n");
                write_clef_xml(
                    &mut out,
                    part_hi.clef_sign.unwrap_or('G'),
                    part_hi.clef_line.unwrap_or(2),
                    part_hi.clef_octave_change,
                    Some(1),
                );
                write_clef_xml(
                    &mut out,
                    part_lo.clef_sign.unwrap_or('F'),
                    part_lo.clef_line.unwrap_or(4),
                    part_lo.clef_octave_change,
                    Some(2),
                );
                out.push_str("      </attributes>\n");
            } else {
                let p1_clef = p1_attrs.filter(|a| {
                    a.clef_sign.is_some() || a.clef_line.is_some() || a.clef_octave_change.is_some()
                });
                let p2_clef = p2_attrs.filter(|a| {
                    a.clef_sign.is_some() || a.clef_line.is_some() || a.clef_octave_change.is_some()
                });
                let key_change = p1_attrs
                    .and_then(|a| a.key_fifths)
                    .or_else(|| p2_attrs.and_then(|a| a.key_fifths))
                    .or_else(|| s_attrs.and_then(|a| a.key_fifths));
                let time_change = s_attrs.and_then(|a| a.time);
                if p1_clef.is_some()
                    || p2_clef.is_some()
                    || key_change.is_some()
                    || time_change.is_some()
                {
                    out.push_str("      <attributes>\n");
                    if let Some(k) = key_change {
                        let _ = writeln!(out, "        <key><fifths>{k}</fifths></key>");
                    }
                    if let Some(t) = time_change {
                        let _ = writeln!(
                            out,
                            "        <time><beats>{}</beats><beat-type>{}</beat-type></time>",
                            t.beats, t.beat_type
                        );
                    }
                    if let Some(a) = p1_clef {
                        write_clef_xml(
                            &mut out,
                            a.clef_sign.unwrap_or('G'),
                            a.clef_line.unwrap_or(2),
                            a.clef_octave_change,
                            Some(1),
                        );
                    }
                    if let Some(a) = p2_clef {
                        write_clef_xml(
                            &mut out,
                            a.clef_sign.unwrap_or('F'),
                            a.clef_line.unwrap_or(4),
                            a.clef_octave_change,
                            Some(2),
                        );
                    }
                    out.push_str("      </attributes>\n");
                }
            }
            let barline_left = s_attrs.and_then(|a| a.barline_left.as_deref());
            let volta = s_attrs.and_then(|a| a.volta.as_deref());
            write_left_barline_xml(&mut out, barline_left, volta);
            let m0 = windows.get(m).map(|&(a, _)| a).unwrap_or(0);
            let mut timed1: Vec<(i64, &TimedEvent)> = Vec::new();
            let mut s_has_timed_dyn = false;
            let mut s_has_timed_hp = false;
            let mut p1_has_timed_dyn = false;
            let mut p1_has_timed_hp = false;
            if let Some(a) = s_attrs {
                for te in &a.timed_events {
                    if te.kind == "dynamic" {
                        s_has_timed_dyn = true;
                    } else if te.kind == "hairpin" {
                        s_has_timed_hp = true;
                    }
                    let tick = (te.onset_beats * score.divisions as f64).round() as i64;
                    timed1.push((tick.max(0), te));
                }
            }
            if let Some(a) = p1_attrs {
                for te in &a.timed_events {
                    if te.kind == "dynamic" {
                        p1_has_timed_dyn = true;
                    } else if te.kind == "hairpin" {
                        p1_has_timed_hp = true;
                    }
                    let tick = (te.onset_beats * score.divisions as f64).round() as i64;
                    timed1.push((tick.max(0), te));
                }
            }
            timed1.sort_by_key(|(tick, _)| *tick);
            if let Some(a) = s_attrs {
                if !s_has_timed_dyn {
                    for d in a.all_dynamics() {
                        write_dyn_direction_xml(&mut out, d, Some(1));
                    }
                }
                if !s_has_timed_hp {
                    for h in a.all_hairpins() {
                        write_hairpin_direction_xml(&mut out, h, Some(1));
                    }
                }
            }
            if let Some(a) = p1_attrs {
                if !p1_has_timed_dyn {
                    for d in a.all_dynamics() {
                        write_dyn_direction_xml(&mut out, d, Some(1));
                    }
                }
                if !p1_has_timed_hp {
                    for h in a.all_hairpins() {
                        write_hairpin_direction_xml(&mut out, h, Some(1));
                    }
                }
            }
            let voices1 = layout_hi.get(m).map(Vec::as_slice).unwrap_or(&[]);
            if voices1.is_empty() {
                for &(_, te) in &timed1 {
                    write_timed_event_xml(&mut out, te, Some(1), None);
                }
            } else {
                for (vi, (_voice, segs)) in voices1.iter().enumerate() {
                    if vi > 0 {
                        let _ = writeln!(out, "      <backup><duration>{mlen}</duration></backup>");
                        for s in segs {
                            if s.forward {
                                push_forward_xml(&mut out, s.dur_ticks, s.note.voice, Some(1));
                            } else {
                                push_note_xml(
                                    &mut out,
                                    s,
                                    score.divisions,
                                    losses,
                                    &part_hi.name,
                                    None,
                                    Some(1),
                                );
                            }
                        }
                    } else {
                        push_voice_with_timed_events(
                            &mut out,
                            segs,
                            m0,
                            score.divisions,
                            losses,
                            &part_hi.name,
                            Some(1),
                            &timed1,
                        );
                    }
                }
            }
            let voices2 = layout_lo.get(m).map(Vec::as_slice).unwrap_or(&[]);
            if !voices1.is_empty() && !voices2.is_empty() {
                let _ = writeln!(out, "      <backup><duration>{mlen}</duration></backup>");
            }
            let mut timed2: Vec<(i64, &TimedEvent)> = Vec::new();
            let mut p2_has_timed_dyn = false;
            let mut p2_has_timed_hp = false;
            if let Some(a) = p2_attrs {
                for te in &a.timed_events {
                    if te.kind == "dynamic" {
                        p2_has_timed_dyn = true;
                    } else if te.kind == "hairpin" {
                        p2_has_timed_hp = true;
                    }
                    let tick = (te.onset_beats * score.divisions as f64).round() as i64;
                    timed2.push((tick.max(0), te));
                }
            }
            timed2.sort_by_key(|(tick, _)| *tick);
            if let Some(a) = p2_attrs {
                if !p2_has_timed_dyn {
                    for d in a.all_dynamics() {
                        write_dyn_direction_xml(&mut out, d, Some(2));
                    }
                }
                if !p2_has_timed_hp {
                    for h in a.all_hairpins() {
                        write_hairpin_direction_xml(&mut out, h, Some(2));
                    }
                }
            }
            if voices2.is_empty() {
                for &(_, te) in &timed2 {
                    write_timed_event_xml(&mut out, te, Some(2), None);
                }
            } else {
                for (vi, (_voice, segs)) in voices2.iter().enumerate() {
                    if vi > 0 {
                        let _ = writeln!(out, "      <backup><duration>{mlen}</duration></backup>");
                        for s in segs {
                            if s.forward {
                                push_forward_xml(&mut out, s.dur_ticks, s.note.voice, Some(2));
                            } else {
                                push_note_xml(
                                    &mut out,
                                    s,
                                    score.divisions,
                                    losses,
                                    &part_lo.name,
                                    None,
                                    Some(2),
                                );
                            }
                        }
                    } else {
                        push_voice_with_timed_events(
                            &mut out,
                            segs,
                            m0,
                            score.divisions,
                            losses,
                            &part_lo.name,
                            Some(2),
                            &timed2,
                        );
                    }
                }
            }
            if let Some(b) = s_attrs.and_then(|a| a.barline_right.as_deref()) {
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
        // hp_start: measure_idx -> Vec<(subtype: 0=cresc|1=dim, dm, df_ticks)>
        // hp_stop_start: measure_idx -> Vec<(dm, df_ticks)> (closed at start of measure)
        // hp_stop_end: measure_idx -> Vec<(dm, df_ticks)> (closed at end of measure)
        let mut hp_start: BTreeMap<usize, Vec<(u8, i64, i64)>> = BTreeMap::new();
        let mut hp_stop_start: BTreeMap<usize, Vec<(i64, i64)>> = BTreeMap::new();
        let mut hp_stop_end: BTreeMap<usize, Vec<(i64, i64)>> = BTreeMap::new();
        let mut open_hp: Option<(usize, u8)> = None;
        for m in 0..n_measures {
            let m1 = m as u32 + 1;
            let mut m_hps: Vec<&str> = Vec::new();
            if let Some(a) = measure_attr(score, m1) {
                m_hps.extend(a.all_hairpins());
            }
            if let Some(a) = part_measure_attr(part, m1) {
                m_hps.extend(a.all_hairpins());
            }
            for h in m_hps {
                match h {
                    "cresc" | "dim" => {
                        if let Some((sm, sub)) = open_hp.take() {
                            if m > sm {
                                let dm = m as i64 - sm as i64;
                                hp_start.entry(sm).or_default().push((sub, dm, 0));
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
                                hp_start.entry(sm).or_default().push((sub, dm, 0));
                                hp_stop_start.entry(m).or_default().push((-dm, 0));
                            } else {
                                let mlen = windows
                                    .get(m)
                                    .map(|&(a, b)| b - a)
                                    .unwrap_or_else(|| score.measure_ticks());
                                hp_start.entry(sm).or_default().push((sub, 0, mlen));
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
                hp_start.entry(sm).or_default().push((sub, dm, mlen));
                hp_stop_end.entry(last).or_default().push((-dm, -mlen));
            }
        }

        let mut cur_sig = (score.beats, score.beat_type);
        let mut cur_key = part.key_fifths.unwrap_or(score.key_fifths);

        for (m, voices) in grouped.iter().enumerate() {
            let m1 = m as u32 + 1;
            let attrs = measure_attr(score, m1);
            let p_attrs = part_measure_attr(part, m1);
            if let Some(a) = attrs {
                if let Some(t) = a.time {
                    cur_sig = (t.beats, t.beat_type);
                }
                if let Some(k) = a.key_fifths {
                    cur_key = k;
                }
            }
            if let Some(a) = p_attrs {
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

            // MuseScore 4 enforces a hard maximum of 4 voices per Staff;
            // emitting a 5th <voice> overflows its internal track array.
            for (vi, (_voice, items)) in voices.iter().take(4).enumerate() {
                out.push_str("        <voice>\n");
                if vi == 0 {
                    // Opening or mid-stream Clef, KeySig, TimeSig belong inside voice 0.
                    if m == 0 {
                        let (clef_s, clef_l, clef_oct) = if let Some(s) = part.clef_sign {
                            (s, part.clef_line.unwrap_or(2), part.clef_octave_change)
                        } else {
                            match attrs {
                                Some(a) => (
                                    a.clef_sign.unwrap_or('G'),
                                    a.clef_line.unwrap_or(2),
                                    a.clef_octave_change,
                                ),
                                None => ('G', 2, None),
                            }
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
                    } else {
                        let clef_src = p_attrs
                            .filter(|a| {
                                a.clef_sign.is_some()
                                    || a.clef_line.is_some()
                                    || a.clef_octave_change.is_some()
                            })
                            .or_else(|| {
                                attrs.filter(|a| {
                                    a.clef_sign.is_some()
                                        || a.clef_line.is_some()
                                        || a.clef_octave_change.is_some()
                                })
                            });
                        if let Some(a) = clef_src {
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
                        if p_attrs.is_some_and(|a| a.key_fifths.is_some())
                            || attrs.is_some_and(|a| a.key_fifths.is_some())
                        {
                            out.push_str("          <KeySig>\n");
                            let _ = writeln!(out, "            <concertKey>{cur_key}</concertKey>");
                            out.push_str("            </KeySig>\n");
                        }
                        if attrs.is_some_and(|a| a.time.is_some()) {
                            out.push_str("          <TimeSig>\n");
                            let _ = writeln!(out, "            <sigN>{}</sigN>", cur_sig.0);
                            let _ = writeln!(out, "            <sigD>{}</sigD>", cur_sig.1);
                            out.push_str("            </TimeSig>\n");
                        }
                    }

                    if let Some(a) = attrs {
                        for d in a.all_dynamics() {
                            out.push_str("          <Dynamic>\n            <subtype>");
                            write_escaped_xml(&mut out, d);
                            out.push_str("</subtype>\n            </Dynamic>\n");
                        }
                    }
                    if let Some(a) = p_attrs {
                        for d in a.all_dynamics() {
                            out.push_str("          <Dynamic>\n            <subtype>");
                            write_escaped_xml(&mut out, d);
                            out.push_str("</subtype>\n            </Dynamic>\n");
                        }
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
                    if let Some(starts) = hp_start.get(&m) {
                        for &(sub, dm, df) in starts {
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
                }

                // Track open <Tuplet> state: (actual, normal, written_acc, target_written).
                let mut open_tuplet: Option<(u32, u32, i64, i64)> = None;

                for (ii, item) in items.iter().enumerate() {
                    let is_measure_rest =
                        item.is_rest && !item.grace && items.len() == 1 && item.dur_ticks == mlen;
                    let tup = if !item.grace && !is_measure_rest {
                        tuplet_duration(item.dur_ticks, score.divisions)
                    } else {
                        None
                    };

                    if let Some((t_name, t_dots, t_base, actual, normal)) = tup {
                        if let Some((oa, on, _, _)) = open_tuplet {
                            if (oa, on) != (actual, normal) {
                                out.push_str("          <endTuplet/>\n");
                                open_tuplet = None;
                            }
                        }
                        if open_tuplet.is_none() {
                            // Look ahead across consecutive same-ratio items to pick the
                            // undotted baseNote and total written span `actual * unit_base`.
                            let (mut unit_name, mut unit_base) =
                                undotted_base_note(t_base, t_dots, score.divisions);
                            let mut acc = 0i64;
                            for next_item in &items[ii..] {
                                if next_item.grace {
                                    continue;
                                }
                                let Some((_, nd, nb, na, nn)) =
                                    tuplet_duration(next_item.dur_ticks, score.divisions)
                                else {
                                    break;
                                };
                                if (na, nn) != (actual, normal) {
                                    break;
                                }
                                let (un, ub) = undotted_base_note(nb, nd, score.divisions);
                                if ub < unit_base {
                                    unit_base = ub;
                                    unit_name = un;
                                }
                                acc += nb;
                                if acc >= actual as i64 * unit_base {
                                    break;
                                }
                            }
                            let target_written = actual as i64 * unit_base;
                            out.push_str("          <Tuplet>\n");
                            let _ = writeln!(out, "            <normalNotes>{normal}</normalNotes>");
                            let _ = writeln!(out, "            <actualNotes>{actual}</actualNotes>");
                            let _ = writeln!(out, "            <baseNote>{unit_name}</baseNote>");
                            out.push_str("            </Tuplet>\n");
                            open_tuplet = Some((actual, normal, 0, target_written));
                        }
                        let _ = t_name;
                    } else if !item.grace && open_tuplet.is_some() {
                        out.push_str("          <endTuplet/>\n");
                        open_tuplet = None;
                    }

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
                        if is_measure_rest {
                            out.push_str("            <durationType>measure</durationType>\n");
                            let _ = writeln!(
                                out,
                                "            <duration>{}/{}</duration>",
                                cur_sig.0, cur_sig.1
                            );
                        } else {
                            let (dtype, dots) = if let Some((tn, td, _, _, _)) = tup {
                                (tn, td)
                            } else {
                                mscx_duration_type(item.dur_ticks, score.divisions)
                            };
                            if dots > 0 {
                                let _ = writeln!(out, "            <dots>{dots}</dots>");
                            }
                            let _ =
                                writeln!(out, "            <durationType>{dtype}</durationType>");
                        }
                        out.push_str("            </Rest>\n");
                        if let (Some((_, _, t_base, _, _)), Some((_, _, acc, target))) =
                            (tup, open_tuplet.as_mut())
                        {
                            *acc += t_base;
                            if *acc >= *target {
                                out.push_str("          <endTuplet/>\n");
                                open_tuplet = None;
                            }
                        }
                        continue;
                    }

                    let (dtype, dots) = if let Some((tn, td, _, _, _)) = tup {
                        (tn, td)
                    } else {
                        mscx_duration_type(item.dur_ticks, score.divisions)
                    };
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
                    if let (Some((_, _, t_base, _, _)), Some((_, _, acc, target))) =
                        (tup, open_tuplet.as_mut())
                    {
                        *acc += t_base;
                        if *acc >= *target {
                            out.push_str("          <endTuplet/>\n");
                            open_tuplet = None;
                        }
                    }
                }
                if open_tuplet.is_some() {
                    out.push_str("          <endTuplet/>\n");
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
            .compression_method(zip::CompressionMethod::Deflated)
            .compression_level(Some(1));
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
            ..Default::default()
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
