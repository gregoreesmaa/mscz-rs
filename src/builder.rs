//! Fluent, ergonomic builders for [`ScoreInput`] and [`PartInput`].
//!
//! [`ScoreBuilder`] and [`PartBuilder`] are re-exported at the crate root
//! (`mscz::ScoreBuilder`, `mscz::PartBuilder`) so you can compose multi-part,
//! multi-voice scores without manually populating [`NoteInput`] structs.
//!
//! # Example
//!
//! ```
//! use mscz::{MeasureAttr, ScoreBuilder};
//!
//! let (score, losses) = ScoreBuilder::new("Two-Voice Prelude")
//!     .composer("J.S. Bach")
//!     .time(4, 4)
//!     .key_fifths(0)
//!     .measure_attr(
//!         1,
//!         MeasureAttr {
//!             dynamic: Some("mp".into()),
//!             ..Default::default()
//!         },
//!     )
//!     .part("Piano", |p| {
//!         p.note("E5", 0.0, 2.0)
//!             .slur_start()
//!             .note("G5", 2.0, 2.0)
//!             .staccato()
//!             .slur_stop()
//!             .voice(2)
//!             .chord(&["C4", "G4"], 0.0, 2.0)
//!             .chord(&["B3", "G4"], 2.0, 2.0)
//!     })
//!     .build()
//!     .expect("valid score");
//!
//! assert!(losses.is_lossless());
//! assert_eq!(score.parts[0].voices, vec![1, 2]);
//! ```

use crate::{
    LossTable, MeasureAttr, NoteInput, PartInput, Score, ScoreInput, TimeSig, SCORE_DIVISIONS,
};

/// Fluent builder for constructing a [`ScoreInput`] or directly parsing a [`Score`].
///
/// Defaults:
/// - `divisions`: [`SCORE_DIVISIONS`] (`480` ticks per quarter note)
/// - `time`: `None` (defaults to `4/4` when parsed)
/// - `key_fifths`: `0` (C major / A minor)
/// - `composer`: `None`
#[derive(Debug, Clone)]
pub struct ScoreBuilder {
    title: String,
    composer: Option<String>,
    divisions: u32,
    time: Option<TimeSig>,
    key_fifths: i32,
    parts: Vec<PartInput>,
    measure_attrs: Vec<MeasureAttr>,
}

impl ScoreBuilder {
    /// Start a new score with the given `title` and default divisions (`480`).
    ///
    /// # Example
    ///
    /// ```
    /// use mscz::ScoreBuilder;
    ///
    /// let input = ScoreBuilder::new("Etude No. 1")
    ///     .part("Piano", |p| p.note("C4", 0.0, 4.0))
    ///     .build_input();
    /// assert_eq!(input.title.as_deref(), Some("Etude No. 1"));
    /// ```
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            composer: None,
            divisions: SCORE_DIVISIONS,
            time: None,
            key_fifths: 0,
            parts: Vec::new(),
            measure_attrs: Vec::new(),
        }
    }

    /// Set the score composer credit.
    pub fn composer(mut self, composer: impl Into<String>) -> Self {
        self.composer = Some(composer.into());
        self
    }

    /// Override quarter-note tick divisions (default: [`SCORE_DIVISIONS`] = `480`).
    ///
    /// Must be greater than `0`; otherwise [`ScoreBuilder::build`] returns an error.
    pub fn divisions(mut self, divisions: u32) -> Self {
        self.divisions = divisions;
        self
    }

    /// Set the initial time signature (e.g. `.time(4, 4)`, `.time(3, 4)`, or `.time(6, 8)`).
    ///
    /// `beats` must be positive and `beat_type` must be a power of two in `1..=32`
    /// (`1`, `2`, `4`, `8`, `16`, or `32`).
    pub fn time(mut self, beats: u32, beat_type: u32) -> Self {
        self.time = Some(TimeSig { beats, beat_type });
        self
    }

    /// Set the initial key signature in fifths (`0` = C major, `1` = G major, `-1` = F major, ...).
    ///
    /// # Example
    ///
    /// ```
    /// use mscz::ScoreBuilder;
    ///
    /// // D major (+2 sharps: F#, C#)
    /// let (score, _) = ScoreBuilder::new("Gavotte")
    ///     .key_fifths(2)
    ///     .part("Violin", |p| p.note("D5", 0.0, 2.0).note("F#5", 2.0, 2.0))
    ///     .build()
    ///     .unwrap();
    /// assert_eq!(score.key_fifths, 2);
    /// ```
    pub fn key_fifths(mut self, fifths: i32) -> Self {
        self.key_fifths = fifths;
        self
    }

    /// Add a named part populated via a [`PartBuilder`] closure.
    ///
    /// Parts appear in the order they are added.
    pub fn part<F>(mut self, name: impl Into<String>, f: F) -> Self
    where
        F: FnOnce(&mut PartBuilder) -> &mut PartBuilder,
    {
        let mut pb = PartBuilder::new(name);
        let _ = f(&mut pb);
        self.parts.push(pb.build());
        self
    }

    /// Attach a measure-scoped notation override (`measure` is 1-based).
    ///
    /// The `measure` argument overwrites `attr.measure`, so you can construct `attr`
    /// with `..MeasureAttr::default()`.
    ///
    /// # Example
    ///
    /// ```
    /// use mscz::{MeasureAttr, ScoreBuilder};
    ///
    /// let (score, _) = ScoreBuilder::new("Dynamic Study")
    ///     .measure_attr(
    ///         1,
    ///         MeasureAttr {
    ///             dynamic: Some("mf".into()),
    ///             barline_right: Some("light-heavy".into()),
    ///             ..Default::default()
    ///         },
    ///     )
    ///     .part("Flute", |p| p.note("C5", 0.0, 4.0))
    ///     .build()
    ///     .unwrap();
    /// assert_eq!(score.measure_attrs[0].measure, 1);
    /// assert_eq!(score.measure_attrs[0].dynamic.as_deref(), Some("mf"));
    /// ```
    pub fn measure_attr(mut self, measure: u32, mut attr: MeasureAttr) -> Self {
        attr.measure = measure;
        self.measure_attrs.push(attr);
        self
    }

    /// Finalize into a raw [`ScoreInput`] without running [`crate::parse_score`].
    ///
    /// Useful when you want to inspect, serialize to JSON, or run
    /// [`crate::validate_notes`] before parsing.
    pub fn build_input(self) -> ScoreInput {
        ScoreInput {
            title: Some(self.title),
            composer: self.composer,
            divisions: self.divisions,
            time: self.time,
            key_fifths: self.key_fifths,
            parts: self.parts,
            notes: Vec::new(),
            part_name: None,
            measure_attrs: self.measure_attrs,
        }
    }

    /// Finalize and parse into a tick-resolved [`Score`] and [`LossTable`].
    ///
    /// # Errors
    ///
    /// Returns `Err(String)` if [`crate::parse_score`] rejects the score, for example when:
    /// - No notes or rests were added across any part.
    /// - `divisions` is `0`, or the time signature has `beats == 0` or an unsupported `beat_type`.
    /// - Any note has a non-finite or out-of-range `onset` / `dur`, `voice == 0`,
    ///   a duration that rounds to `0` ticks, or an invalid / out-of-MIDI-range pitch string.
    ///
    /// # Example
    ///
    /// ```
    /// use mscz::ScoreBuilder;
    ///
    /// // An empty score fails to parse.
    /// let err = ScoreBuilder::new("Empty").build().unwrap_err();
    /// assert!(err.contains("no notes"));
    /// ```
    pub fn build(self) -> Result<(Score, LossTable), String> {
        let input = self.build_input();
        crate::parse_score(&input)
    }
}

/// Fluent builder for one named [`PartInput`].
///
/// Modifier methods (`.staccato()`, `.accent()`, `.tenuto()`, `.fermata()`,
/// `.tie_start()`, `.tie_stop()`, `.slur_start()`, `.slur_stop()`, `.lyric(...)`,
/// `.grace()`) apply to the most recently added note or rest (and are a no-op if
/// no note has been added yet).
///
/// Simultaneous notes in the same voice share a [`crate::ChordGroup`] automatically;
/// use [`PartBuilder::chord`] to emit a multi-note chord in a single call.
#[derive(Debug, Clone)]
pub struct PartBuilder {
    name: String,
    current_voice: u32,
    notes: Vec<NoteInput>,
}

impl PartBuilder {
    /// Start a new part builder defaulting to voice `1`.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            current_voice: 1,
            notes: Vec::new(),
        }
    }

    /// Set the 1-based voice for subsequent notes, chords, and rests.
    ///
    /// Voice numbers must be `>= 1` when parsed by [`crate::parse_score`].
    pub fn voice(&mut self, voice: u32) -> &mut Self {
        self.current_voice = voice;
        self
    }

    /// Append a pitched note at `onset` (beats) with duration `dur` (beats).
    ///
    /// `pitch` uses scientific pitch notation (see [`crate::pitch::parse_pitch`]),
    /// such as `"C4"`, `"F#5"`, `"Bb3"`, `"Gx2"`, or `"A4+30c"`.
    pub fn note(&mut self, pitch: impl Into<String>, onset: f64, dur: f64) -> &mut Self {
        self.notes.push(NoteInput {
            pitch: Some(pitch.into()),
            rest: false,
            onset,
            dur,
            voice: self.current_voice,
            lyric: None,
            grace: false,
            slur_start: false,
            slur_stop: false,
            articulations: Vec::new(),
            tie_start: false,
            tie_stop: false,
        });
        self
    }

    /// Append a simultaneous chord of `pitches` at `onset` (beats) with duration `dur` (beats)
    /// in the current voice.
    ///
    /// Each pitch is emitted as a [`NoteInput`] at the same `(onset, dur, voice)` so
    /// [`crate::parse_score`] groups them into one [`crate::ChordGroup`]. Passing an
    /// empty slice is a no-op.
    ///
    /// # Example
    ///
    /// ```
    /// use mscz::ScoreBuilder;
    ///
    /// let (score, losses) = ScoreBuilder::new("Triad")
    ///     .part("Piano", |p| p.chord(&["C4", "E4", "G4"], 0.0, 4.0))
    ///     .build()
    ///     .unwrap();
    ///
    /// assert!(losses.is_lossless());
    /// assert_eq!(score.parts[0].groups.len(), 1);
    /// assert_eq!(score.parts[0].groups[0].members.len(), 3);
    /// ```
    pub fn chord(&mut self, pitches: &[&str], onset: f64, dur: f64) -> &mut Self {
        for &pitch in pitches {
            self.note(pitch, onset, dur);
        }
        self
    }

    /// Append a grace note at `onset` (beats) with engraved display duration `dur` (beats).
    ///
    /// Shorthand for `self.note(pitch, onset, dur).grace()`. Grace notes occupy zero
    /// musical time; `dur` only controls the engraved note type (e.g. `0.5` for an eighth).
    ///
    /// # Example
    ///
    /// ```
    /// use mscz::ScoreBuilder;
    ///
    /// let (score, _) = ScoreBuilder::new("Ornament")
    ///     .part("Violin", |p| {
    ///         p.grace_note("D5", 0.0, 0.5)
    ///             .note("C5", 0.0, 1.0)
    ///     })
    ///     .build()
    ///     .unwrap();
    ///
    /// assert!(score.parts[0].groups[0].members[0].grace);
    /// ```
    pub fn grace_note(&mut self, pitch: impl Into<String>, onset: f64, dur: f64) -> &mut Self {
        self.note(pitch, onset, dur).grace()
    }

    /// Append a rest at `onset` (beats) with duration `dur` (beats).
    pub fn rest(&mut self, onset: f64, dur: f64) -> &mut Self {
        self.notes.push(NoteInput {
            pitch: None,
            rest: true,
            onset,
            dur,
            voice: self.current_voice,
            lyric: None,
            grace: false,
            slur_start: false,
            slur_stop: false,
            articulations: Vec::new(),
            tie_start: false,
            tie_stop: false,
        });
        self
    }

    fn push_articulation(&mut self, kind: &str) -> &mut Self {
        if let Some(last) = self.notes.last_mut() {
            if !last.articulations.iter().any(|a| a == kind) {
                last.articulations.push(kind.to_string());
            }
        }
        self
    }

    /// Mark the most recent note with a staccato articulation (`"stacc"`).
    pub fn staccato(&mut self) -> &mut Self {
        self.push_articulation("stacc")
    }

    /// Mark the most recent note with an accent articulation (`"accent"`).
    pub fn accent(&mut self) -> &mut Self {
        self.push_articulation("accent")
    }

    /// Mark the most recent note with a tenuto articulation (`"tenuto"`).
    pub fn tenuto(&mut self) -> &mut Self {
        self.push_articulation("tenuto")
    }

    /// Mark the most recent note with a fermata (`"fermata"`).
    pub fn fermata(&mut self) -> &mut Self {
        self.push_articulation("fermata")
    }

    /// Start a tie on the most recent note.
    pub fn tie_start(&mut self) -> &mut Self {
        if let Some(last) = self.notes.last_mut() {
            last.tie_start = true;
        }
        self
    }

    /// Stop a tie on the most recent note.
    pub fn tie_stop(&mut self) -> &mut Self {
        if let Some(last) = self.notes.last_mut() {
            last.tie_stop = true;
        }
        self
    }

    /// Start a slur on the most recent note.
    pub fn slur_start(&mut self) -> &mut Self {
        if let Some(last) = self.notes.last_mut() {
            last.slur_start = true;
        }
        self
    }

    /// Stop a slur on the most recent note.
    pub fn slur_stop(&mut self) -> &mut Self {
        if let Some(last) = self.notes.last_mut() {
            last.slur_stop = true;
        }
        self
    }

    /// Attach a lyric syllable to the most recent note.
    pub fn lyric(&mut self, text: impl Into<String>) -> &mut Self {
        if let Some(last) = self.notes.last_mut() {
            last.lyric = Some(text.into());
        }
        self
    }

    /// Mark the most recent note as a grace note.
    pub fn grace(&mut self) -> &mut Self {
        if let Some(last) = self.notes.last_mut() {
            last.grace = true;
        }
        self
    }

    /// Consume the builder into a [`PartInput`].
    pub fn build(self) -> PartInput {
        PartInput {
            name: self.name,
            notes: self.notes,
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_two_voice_score_input_and_parses() {
        let input = ScoreBuilder::new("Two-Voice Prelude")
            .composer("J.S. Bach")
            .time(4, 4)
            .key_fifths(0)
            .part("Piano", |p| {
                p.note("E5", 0.0, 2.0)
                    .note("G5", 2.0, 2.0)
                    .voice(2)
                    .note("C4", 0.0, 1.0)
                    .note("G4", 1.0, 3.0)
            })
            .build_input();

        assert_eq!(input.title.as_deref(), Some("Two-Voice Prelude"));
        assert_eq!(input.composer.as_deref(), Some("J.S. Bach"));
        assert_eq!(
            input.time,
            Some(TimeSig {
                beats: 4,
                beat_type: 4
            })
        );
        assert_eq!(input.key_fifths, 0);
        assert_eq!(input.parts.len(), 1);
        assert_eq!(input.parts[0].name, "Piano");
        assert_eq!(input.parts[0].notes.len(), 4);
        assert_eq!(input.parts[0].notes[0].voice, 1);
        assert_eq!(input.parts[0].notes[2].voice, 2);

        let (score, losses) = crate::parse_score(&input).expect("parse_score");
        assert!(losses.is_lossless());
        assert_eq!(score.parts.len(), 1);
        assert_eq!(score.parts[0].voices, vec![1, 2]);
    }

    #[test]
    fn chord_and_grace_note_helpers_group_correctly() {
        let (score, losses) = ScoreBuilder::new("Chord Study")
            .time(4, 4)
            .part("Piano", |p| {
                p.grace_note("B4", 0.0, 0.5)
                    .chord(&["C5", "E5", "G5"], 0.0, 2.0)
                    .chord(&[], 2.0, 2.0) // empty chord is a no-op
                    .voice(2)
                    .chord(&["C3", "G3"], 0.0, 4.0)
            })
            .build()
            .expect("build");

        assert!(losses.is_lossless());
        assert_eq!(score.parts[0].voices, vec![1, 2]);
        let v1_groups: Vec<_> = score.parts[0]
            .groups
            .iter()
            .filter(|g| g.voice == 1)
            .collect();
        assert_eq!(v1_groups.len(), 2); // grace group + triad group
        assert!(v1_groups[0].members[0].grace);
        assert_eq!(v1_groups[1].members.len(), 3);

        let v2_groups: Vec<_> = score.parts[0]
            .groups
            .iter()
            .filter(|g| g.voice == 2)
            .collect();
        assert_eq!(v2_groups.len(), 1);
        assert_eq!(v2_groups[0].members.len(), 2);
    }

    #[test]
    fn modifiers_and_measure_attrs_roundtrip() {
        let (score, losses) = ScoreBuilder::new("Articulated Study")
            .time(3, 4)
            .key_fifths(-1)
            .measure_attr(
                1,
                MeasureAttr {
                    dynamic: Some("mf".to_string()),
                    ..Default::default()
                },
            )
            .part("Voice", |p| {
                p.note("D4", 0.0, 0.5)
                    .grace()
                    .note("E4", 0.0, 1.0)
                    .staccato()
                    .slur_start()
                    .lyric("La")
                    .note("F4", 1.0, 1.0)
                    .accent()
                    .tenuto()
                    .tie_start()
                    .slur_stop()
                    .note("F4", 2.0, 1.0)
                    .tie_stop()
                    .fermata()
                    .rest(3.0, 3.0)
            })
            .build()
            .expect("build");

        assert!(losses.is_lossless());
        assert_eq!(score.beats, 3);
        assert_eq!(score.beat_type, 4);
        assert_eq!(score.key_fifths, -1);
        assert_eq!(score.measure_attrs.len(), 1);
        assert_eq!(score.measure_attrs[0].measure, 1);
        assert_eq!(score.measure_attrs[0].dynamic.as_deref(), Some("mf"));

        let notes: Vec<_> = score.parts[0]
            .groups
            .iter()
            .flat_map(|g| g.members.iter())
            .collect();
        assert_eq!(notes.len(), 5);
        assert!(notes[0].grace);
        assert_eq!(notes[1].articulations, vec!["stacc"]);
        assert!(notes[1].slur_start);
        assert_eq!(notes[1].lyric.as_deref(), Some("La"));
        assert_eq!(notes[2].articulations, vec!["accent", "tenuto"]);
        assert!(notes[2].tie_start);
        assert!(notes[2].slur_stop);
        assert!(notes[3].tie_stop);
        assert_eq!(notes[3].articulations, vec!["fermata"]);
        assert!(notes[4].rest);
    }
}
