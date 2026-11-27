//! Canonical `bekern+` token stream -> [`ScoreInput`] decoder.
//!
//! Inverse of [`to_bekern`](crate::bekern::to_bekern): reconstructs
//! absolute performed onsets by accumulating written durations per
//! `voice-N` segment, scaling contributors inside open
//! `tuplet-start-A-N` spans by N/A. Mid-stream `clef`/`key`/`time`
//! tokens, measure dynamics/hairpins, barline variants, `grace` notes
//! and slur/articulation/fermata modifiers all parse into
//! [`ScoreInput`] (notes plus `measure_attrs`).
//! `bar-rest` jumps its voice cursor to the bar end. Voice assignment
//! is exact (markers are normative); a stream with missing markers
//! degrades to single-segment accumulation, never a crash.
//!
//! Tie handling differs by consumer (see [`decode_bekern`] vs
//! [`decode_bekern_unmerged`]): the note-list path merges chains
//! (its reference adapter merges too), while the MusicXML path
//! preserves every link (the XML scorer compares tie segments).

use crate::{MeasureAttr, NoteInput, PartInput, ScoreInput, TimedEvent, TimeSig, SCORE_DIVISIONS};

fn written_dur(token: &str) -> Option<f64> {
    match token {
        "breve" => Some(8.0),
        "whole" => Some(4.0),
        "half" => Some(2.0),
        "quarter" => Some(1.0),
        "eighth" => Some(0.5),
        "16th" => Some(0.25),
        "32nd" => Some(0.125),
        "64th" => Some(0.0625),
        "128th" => Some(0.03125),
        _ => None,
    }
}

fn alter_suffix(alter: i32) -> Result<&'static str, String> {
    match alter {
        0 => Ok(""),
        1 => Ok("#"),
        2 => Ok("x"),
        -1 => Ok("b"),
        -2 => Ok("bb"),
        _ => Err(format!("unsupported alter {alter}")),
    }
}

/// Slur + modifier marks carried on one note (shared by the merged
/// and unmerged decode paths).
#[derive(Debug, Clone, Default)]
struct NoteMods {
    slur_start: bool,
    slur_stop: bool,
    articulations: Vec<String>,
    cue: bool,
    xstaff: Option<i8>,
}

impl NoteMods {
    /// Union of two links' marks (merged tie chains keep every mark).
    fn union(&self, other: &NoteMods) -> NoteMods {
        let mut articulations = self.articulations.clone();
        for a in &other.articulations {
            if !articulations.contains(a) {
                articulations.push(a.clone());
            }
        }
        NoteMods {
            slur_start: self.slur_start || other.slur_start,
            slur_stop: self.slur_stop || other.slur_stop,
            articulations,
            cue: self.cue || other.cue,
            xstaff: self.xstaff.or(other.xstaff),
        }
    }
}

fn mk_note(
    pitch: String,
    onset: f64,
    dur: f64,
    voice: u32,
    mods: &NoteMods,
    tuplet: Option<(u32, u32)>,
) -> NoteInput {
    NoteInput {
        pitch: Some(pitch),
        rest: false,
        onset,
        dur,
        voice,
        lyric: None,
        grace: false,
        slur_start: mods.slur_start,
        slur_stop: mods.slur_stop,
        articulations: mods.articulations.clone(),
        tie_start: false,
        tie_stop: false,
        tuplet,
        cue: mods.cue,
        xstaff: mods.xstaff,
    }
}

/// Consume one slur/modifier tail mark, if the head token is one.
fn take_mod(toks: &[&str], i: &mut usize, mods: &mut NoteMods) -> bool {
    match toks.get(*i).copied() {
        Some("slur-start") => {
            mods.slur_start = true;
            *i += 1;
            true
        }
        Some("slur-stop") => {
            mods.slur_stop = true;
            *i += 1;
            true
        }
        Some("cue") => {
            mods.cue = true;
            *i += 1;
            true
        }
        Some("xstaff-up") => {
            mods.xstaff = Some(-1);
            *i += 1;
            true
        }
        Some("xstaff-down") => {
            mods.xstaff = Some(1);
            *i += 1;
            true
        }
        Some(s)
            if matches!(
                s,
                "stacc"
                    | "accent"
                    | "tenuto"
                    | "fermata"
                    | "art-arpeggiate"
                    | "art-trill"
                    | "art-tremolo"
                    | "art-mordent"
                    | "art-turn"
                    | "art-staccatissimo"
                    | "art-up-bow"
                    | "art-down-bow"
                    | "art-soft-accent"
            ) =>
        {
            if !mods.articulations.iter().any(|a| a == s) {
                mods.articulations.push(s.to_string());
            }
            *i += 1;
            true
        }
        _ => false,
    }
}

/// Consume a pitched note's tail: slur/modifier and tie marks in any
/// interleave (encoder order is slurs, modifiers, then ties).
fn take_tail(
    toks: &[&str],
    i: &mut usize,
    mods: &mut NoteMods,
    tie_start: &mut bool,
    tie_stop: &mut bool,
) {
    loop {
        if take_mod(toks, i, mods) {
            continue;
        }
        match toks.get(*i).copied() {
            Some("tie-start") => {
                *tie_start = true;
                *i += 1;
            }
            Some("tie-stop") => {
                *tie_stop = true;
                *i += 1;
            }
            _ => break,
        }
    }
}

/// Consume a rest's tail: slur/modifier marks only (a tie mark ends
/// the tail and errors downstream as an unexpected token).
fn take_tail_rest(toks: &[&str], i: &mut usize, mods: &mut NoteMods) {
    while take_mod(toks, i, mods) {}
}

/// Mutable `measure_attrs` slot for one 1-based measure (created on
/// first use; callers sort by measure at the end).
fn attr_for(attrs: &mut Vec<MeasureAttr>, measure_1based: u32) -> &mut MeasureAttr {
    if attrs.last().is_some_and(|a| a.measure == measure_1based) {
        return attrs.last_mut().expect("just checked");
    }
    if let Some(i) = attrs.iter().position(|a| a.measure == measure_1based) {
        return &mut attrs[i];
    }
    attrs.push(MeasureAttr {
        measure: measure_1based,
        ..Default::default()
    });
    attrs.last_mut().expect("just pushed")
}

/// Parse a `clef-<sign>-<line>` or `clef-<sign>-<line>-8vb|8va` token.
fn parse_clef_token(t: &str) -> Result<(char, u32, Option<i32>), String> {
    let body = t
        .strip_prefix("clef-")
        .ok_or_else(|| format!("bad clef token {t:?}"))?;
    let mut parts = body.split('-');
    let sign = parts
        .next()
        .and_then(|s| s.chars().next())
        .ok_or_else(|| format!("bad clef token {t:?}"))?;
    let line: u32 = parts
        .next()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| format!("bad clef token {t:?}"))?;
    let oct = match parts.next() {
        None => None,
        Some("8vb") => Some(-1),
        Some("8va") => Some(1),
        Some(_) => return Err(format!("bad clef token {t:?}")),
    };
    if parts.next().is_some() {
        return Err(format!("bad clef token {t:?}"));
    }
    Ok((sign, line, oct))
}

/// Resolve the 0-based voice segment owning a mid-stream clef/dynamic/hairpin token.
///
/// `to_bekern` emits bound attributes/directions immediately BEFORE the `voice-N`
/// marker of the segment they attach to, while hand-authored or model-predicted
/// streams may place them AFTER `voice-N`. Scanning past adjacent bound tokens for
/// an immediately following `voice-N` handles both orders accurately.
fn resolve_target_seg(toks: &[&str], idx_after: usize, current_seg: usize) -> usize {
    let mut j = idx_after;
    while let Some(&tok) = toks.get(j) {
        if let Some(rest) = tok.strip_prefix("voice-") {
            if let Ok(n) = rest.parse::<usize>() {
                if n >= 1 {
                    return n - 1;
                }
            }
            break;
        }
        if tok.starts_with("clef-")
            || tok.starts_with("key")
            || tok.starts_with("time-")
            || tok.starts_with("dyn-")
            || tok.starts_with("hairpin-")
            || tok.starts_with("ottava-")
            || tok == "bar-repeat-left"
            || tok == "volta-1"
            || tok == "volta-2"
        {
            j += 1;
            continue;
        }
        break;
    }
    current_seg
}

/// Closing-barline short style for a `bar-*` variant token.
fn barline_right_of(t: &str) -> Option<&'static str> {
    match t {
        "bar-light-heavy" => Some("light-heavy"),
        "bar-repeat-right" => Some("repeat-right"),
        "bar-double" => Some("double"),
        _ => None,
    }
}

/// Apply tie open/close to one pitched note. Returns the note to emit,
/// or `None` when the note is swallowed into an open chain (emitted on
/// close or end-of-stream flush). Same-note start+stop with no open
/// chain emits once (degenerate input, never produced by the encoder).
/// Apply tie open/close to one pitched note. Returns the note to emit,
/// or `None` when the note is swallowed into an open chain (emitted on
/// close or end-of-stream flush). Same-note start+stop with no open
/// chain emits once (degenerate input, never produced by the encoder).
/// Chains are keyed by pitch so parallel tied voices/chord tones merge
/// independently (a single slot would drop all but the last chain).
/// Per-pitch open-chain stack of `(seg, onset, dur_acc)`: parallel
/// tied voices merge independently, including unison doublings where
/// two voices tie the same pitch at once (ds0462 m19 — pitch-only
/// keying let the second tie-start overwrite the first). A tie_stop
/// takes the topmost same-segment entry (exact within a bar: segments
/// are stable per measure). Stops with no same-seg entry take the
/// single open entry when exactly one exists (the ordinary churned
/// cross-bar chain). Empty stack (or several opens with no same-seg
/// match) stays literal: page-cut stops, or adversarial overlaps
/// resolved LIFO-deterministically by taking the stack top. A
/// tie_start on a segment that already has an open entry overwrites it
/// (recency — mirrors the oracle's single-slot merge per (staff,
/// voice, pitch), which drops the superseded head). Stack order is
/// push order (oldest first), so the end-of-stream flush and every
/// fallback are deterministic.
/// Natural center MIDI pitch for a clef, used to partition `V > K` voice
/// segments monotonically across `K` staves in multi-staff `bekern+` streams.
fn clef_center_midi(sign: char, line: u32, oct: Option<i32>) -> f64 {
    let base = match (sign, line) {
        ('G', 1) => 74.0, // French violin
        ('G', _) => 71.0, // Treble (B4)
        ('C', 1) => 67.0, // Soprano (G4)
        ('C', 2) => 64.0, // Mezzo-soprano (E4)
        ('C', 3) => 60.0, // Alto (C4)
        ('C', 4) => 57.0, // Tenor (A3)
        ('C', 5) => 53.0, // Baritone C (F3)
        ('F', 3) => 53.0, // Baritone F
        ('F', 5) => 47.0, // Sub-bass
        ('F', _) => 50.0, // Bass (D3)
        _ => 64.0,
    };
    base + 12.0 * oct.unwrap_or(0) as f64
}

/// Monotonically map `0..v` voice segments in one measure onto `0..k` staves,
/// returning `(staff_idx, local_voice_1based)` for each segment.
///
/// `to_bekern` orders `(staff, voice)` in strictly ascending staff order, so:
/// - When `v <= k`, segment `s` maps 1-to-1 to staff `s`, local voice `1`.
/// - When `v > k`, a small DP finds the minimum-cost monotonic contiguous
///   partition of `0..v` into `k` non-empty staff blocks matching each staff's
///   clef register, numbering local voices `1..=4` within each staff.
/// - When multiple staves share identical clef centers (e.g. 6-staff SATB+Piano
///   all in `clef-G-2`, or 3-staff voice+piano `G-2, G-2, F-4`), excess
///   polyphonic voices are spread quadratically and biased toward the bottom /
///   later staves (e.g. piano RH/LH) rather than collapsing into Staff 1, with
///   gentle cross-measure pitch continuity from `prev_staff_midi`.
fn assign_segments_to_staves(
    seg_mean_midi: &[Option<f64>],
    clefs: &[(char, u32, Option<i32>)],
    prev_staff_midi: &[Option<f64>],
) -> Vec<(usize, u32)> {
    let v = seg_mean_midi.len();
    let k = clefs.len();
    if v == 0 || k <= 1 {
        return (0..v).map(|s| (0, s as u32 + 1)).collect();
    }
    if v <= k {
        return (0..v).map(|s| (s, 1)).collect();
    }

    let centers: Vec<f64> = clefs
        .iter()
        .map(|&(sign, line, oct)| clef_center_midi(sign, line, oct))
        .collect();

    // dp[s][i] = min cost to partition segments 0..i across staves 0..s (1-indexed counts).
    let inf = f64::INFINITY;
    let mut dp = vec![vec![inf; v + 1]; k + 1];
    let mut prev = vec![vec![0usize; v + 1]; k + 1];
    dp[0][0] = 0.0;

    for s in 1..=k {
        let center = centers[s - 1];
        let prev_midi = prev_staff_midi.get(s - 1).copied().flatten();
        for i in s..=(v - (k - s)) {
            let mut seg_cost = 0.0;
            for j in (s - 1..i).rev() {
                if let Some(m) = seg_mean_midi[j] {
                    seg_cost += (m - center).abs();
                    if let Some(pm) = prev_midi {
                        seg_cost += (m - pm).abs() * 0.15;
                    }
                }
                let block_len = i - j;
                let extra = (block_len - 1) as f64;
                // Hard preference for staying <= 4 voices/staff (MuseScore track limit).
                let over_cap = if block_len > 4 {
                    (block_len - 4) as f64 * 500.0
                } else {
                    0.0
                };
                // Convex balance penalty (quadratic so spreading excess voices across
                // staves strictly beats piling them into one staff) + bottom-staff bias
                // so same-clef scores (e.g. SATB+Piano or Voice+Piano) merge extra
                // polyphonic voices onto the lower/piano staves instead of Staff 1.
                let balance = extra * extra * 0.5;
                let staff_pos_bias = extra * ((k - s) as f64) * 0.35;
                let cand = dp[s - 1][j] + seg_cost + balance + staff_pos_bias + over_cap;
                if cand < dp[s][i] {
                    dp[s][i] = cand;
                    prev[s][i] = j;
                }
            }
        }
    }

    let mut out = vec![(0usize, 1u32); v];
    let mut cur = v;
    for s in (1..=k).rev() {
        let start = prev[s][cur];
        for (offset, slot) in out[start..cur].iter_mut().enumerate() {
            *slot = (s - 1, (offset + 1).min(4) as u32);
        }
        cur = start;
    }
    out
}

/// Open tie-chain state keyed by `(step, octave, alter)`:
/// `(start_measure, staff_idx, seg, onset, dur_acc, mods, tuplet)`.
type PendingTies = std::collections::BTreeMap<
    (char, i32, i32),
    Vec<(u32, usize, usize, f64, f64, NoteMods, Option<(u32, u32)>)>,
>;

#[allow(clippy::too_many_arguments)]
fn tie_note(
    pending: &mut PendingTies,
    key: (char, i32, i32),
    pitch: String,
    onset: f64,
    performed: f64,
    voice: u32,
    measure: u32,
    staff_idx: usize,
    seg: usize,
    tie_start: bool,
    tie_stop: bool,
    mods: &NoteMods,
    tuplet: Option<(u32, u32)>,
) -> Option<(u32, usize, NoteInput)> {
    if tie_stop {
        let mut stack = pending.remove(&key).unwrap_or_default();
        let i = stack
            .iter()
            .rposition(|(_, st, s, _, _, _, _)| *st == staff_idx && *s == seg)
            .or(if stack.len() == 1 {
                Some(0)
            } else if stack.len() > 1 {
                Some(stack.len() - 1)
            } else {
                None
            });
        match i {
            Some(i) => {
                let (sm, sst, _, o, acc, head_mods, head_tup) = stack.remove(i);
                if !stack.is_empty() {
                    pending.insert(key, stack);
                }
                let total = acc + performed;
                let merged = head_mods.union(mods);
                let out_tup = if head_tup == tuplet { head_tup } else { None };
                if tie_start {
                    pending
                        .entry(key)
                        .or_default()
                        .push((sm, sst, seg, o, total, merged, out_tup));
                    return None;
                }
                return Some((sm, sst, mk_note(pitch, o, total, voice, &merged, out_tup)));
            }
            None => {
                debug_assert!(stack.is_empty());
                return Some((
                    measure,
                    staff_idx,
                    mk_note(pitch, onset, performed, voice, mods, tuplet),
                ));
            }
        }
    }
    if tie_start {
        let mut stack = pending.remove(&key).unwrap_or_default();
        stack.retain(|(_, st, s, _, _, _, _)| !(*st == staff_idx && *s == seg));
        stack.push((measure, staff_idx, seg, onset, performed, mods.clone(), tuplet));
        pending.insert(key, stack);
        return None;
    }
    Some((
        measure,
        staff_idx,
        mk_note(pitch, onset, performed, voice, mods, tuplet),
    ))
}

/// Decode a bekern page to a single-part [`ScoreInput`], merging tie
/// chains into single notes (link-faithful input stays link-faithful:
/// each chain keeps every link's slur/modifier marks by union).
///
/// This is the merged note-list path: its reference adapter
/// ([`score_input_from_musicxml`](crate::musicxml_import::score_input_from_musicxml))
/// merges ties too, so both sides agree (grace notes are dropped on
/// both sides as well). For link-faithful MusicXML output — the
/// strict-xml oracle compares tie segments — use
/// [`decode_bekern_unmerged`].
///
/// Voice assignment follows the normative `voice-N` markers (appearance
/// order per measure); a stream with missing markers degrades to
/// single-segment accumulation.
pub fn decode_bekern(tokens: &str) -> Result<ScoreInput, String> {
    decode_inner(tokens, true)
}

/// Decode a bekern page to a single-part [`ScoreInput`] preserving
/// every tie link as its own note (with `tie_start`/`tie_stop` flags
/// for the renderer). Grace notes are kept (zero-width, display
/// duration only). Used by `bekern2musicxml`, whose strict-xml oracle
/// compares tie segments rather than merged notes.
pub fn decode_bekern_unmerged(tokens: &str) -> Result<ScoreInput, String> {
    decode_inner(tokens, false)
}

/// Shared decode core: `merge_ties` selects the note-list semantics
/// ([`decode_bekern`]) vs the notation-faithful semantics
/// ([`decode_bekern_unmerged`]). Token parsing, cursors, spans,
/// measure attributes and error behavior are identical.
fn decode_inner(tokens: &str, merge_ties: bool) -> Result<ScoreInput, String> {
    let toks: Vec<&str> = tokens.split_whitespace().collect();
    let mut key_fifths = 0i32;
    let mut time = TimeSig {
        beats: 4,
        beat_type: 4,
    };
    let mut time_set = false;
    let mut leading_clefs: Vec<(char, u32, Option<i32>)> = Vec::new();
    let mut leading_keys: Vec<Option<i32>> = Vec::new();
    let mut leading_clef_oct: Option<(char, u32, i32)> = None;
    let mut i = 0;
    while i < toks.len() {
        let t = toks[i];
        if t.starts_with("clef-") {
            let (sign, line, oct) = parse_clef_token(t)?;
            leading_clefs.push((sign, line, oct));
            leading_keys.push(None);
            if let Some(o) = oct {
                if leading_clef_oct.is_none() {
                    leading_clef_oct = Some((sign, line, o));
                }
            }
            i += 1;
        } else if let Some(rest) = t.strip_prefix("key") {
            let fifths = rest
                .parse::<i32>()
                .map_err(|_| format!("bad key token {t:?}"))?;
            if let Some(slot) = leading_keys.last_mut() {
                *slot = Some(fifths);
                if leading_keys.len() == 1 {
                    key_fifths = fifths;
                }
            } else {
                key_fifths = fifths;
            }
            i += 1;
        } else if let Some(rest) = t.strip_prefix("time-") {
            if !time_set {
                let mut parts = rest.split('-');
                let beats: u32 = parts
                    .next()
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| format!("bad time token {t:?}"))?;
                let beat_type: u32 = parts
                    .next()
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| format!("bad time token {t:?}"))?;
                time = TimeSig { beats, beat_type };
                time_set = true;
            }
            i += 1;
        } else {
            break;
        }
    }

    let mut tagged_notes: Vec<(u32, usize, NoteInput)> = Vec::new();
    let mut measure_attrs: Vec<MeasureAttr> = Vec::new();
    let mut seg_clefs: Vec<(u32, Option<usize>, usize, char, u32, Option<i32>)> = Vec::new();
    let mut seg_keys: Vec<(u32, Option<usize>, usize, i32)> = Vec::new();
    let mut seg_dyns: Vec<(u32, Option<usize>, usize, f64, String)> = Vec::new();
    let mut seg_hairpins: Vec<(u32, Option<usize>, usize, f64, String)> = Vec::new();
    let mut seg_timed: Vec<(u32, Option<usize>, usize, TimedEvent)> = Vec::new();
    let mut bar_rest_count: usize = 0;
    let mut has_explicit_staff = false;
    let mut cur_staff_idx: usize = 0;
    let mut max_staff_idx: usize = 0;
    if leading_clefs.len() <= 1 {
        if let Some((sign, line, oct)) = leading_clef_oct {
            let a = attr_for(&mut measure_attrs, 1);
            a.clef_sign = Some(sign);
            a.clef_line = Some(line);
            a.clef_octave_change = Some(oct);
        }
    }
    let mut measure = 0u32;
    // Per-measure voice cursors (offsets within the measure); segment 0
    // is created lazily by the first event.
    let mut voices: Vec<f64> = vec![0.0];
    let mut seg = 0usize;
    // Last placed note onset per segment (for `chord` tones).
    let mut last_onset: Vec<Option<f64>> = vec![None];
    // Open tuplet ratios per segment (mirrors to_bekern's per-key
    // stacks: within a measure, segment <-> (staff, voice) is bijective,
    // and spans never cross `bar` by encoder construction). Pending
    // starts queue up (nested reopens) and drain into the next event.
    // Invariant: a `tuplet-start` immediately precedes its member event
    // (the encoder never emits `start … voice-X … event`); the grammar
    // mask is documented wider-than-encoder, so a generator that
    // separates a start from its event by a voice marker would misroute
    // the drain — latent, no behavior change.
    let mut spans: Vec<Vec<(u32, u32)>> = vec![Vec::new()];
    let mut pending_spans: Vec<(u32, u32)> = Vec::new();
    // Open tie chain: ((step, octave, alter), seg, onset, dur_acc,
    // marks). Only used when `merge_ties` is set.
    let mut pending_tie: PendingTies = std::collections::BTreeMap::new();
    // Grace state: `grace_pending` is set by a `grace` marker and
    // consumed by the next duration token; `grace_run` marks chord
    // tones inheriting their root's grace.
    let mut grace_pending = false;
    let mut grace_run = false;
    // Whether any note or rest has been placed: mid-stream headers
    // before the first event are effectively leading headers.
    let mut seen_event = false;

    let scale_of = |spans: &[Vec<(u32, u32)>], s: usize| -> f64 {
        spans[s]
            .iter()
            .fold(1.0, |acc, &(a, n)| acc * n as f64 / a as f64)
    };
    let ratio_of = |spans: &[Vec<(u32, u32)>], s: usize| -> Option<(u32, u32)> {
        let stack = &spans[s];
        if stack.is_empty() {
            return None;
        }
        if stack.len() == 1 {
            return Some(stack[0]);
        }
        let mut num = 1u64;
        let mut den = 1u64;
        for &(a, n) in stack {
            num = num.saturating_mul(a as u64);
            den = den.saturating_mul(n as u64);
        }
        Some((num.min(u32::MAX as u64) as u32, den.min(u32::MAX as u64) as u32))
    };
    let push_rest = |notes: &mut Vec<(u32, usize, NoteInput)>,
                     m: u32,
                     st: usize,
                     onset: f64,
                     dur: f64,
                     voice: u32,
                     mods: &NoteMods,
                     grace: bool,
                     tuplet: Option<(u32, u32)>| {
        notes.push((
            m,
            st,
            NoteInput {
                pitch: None,
                rest: true,
                onset,
                dur,
                voice,
                lyric: None,
                grace,
                slur_start: mods.slur_start,
                slur_stop: mods.slur_stop,
                articulations: mods.articulations.clone(),
                tie_start: false,
                tie_stop: false,
                tuplet,
                cue: mods.cue,
                xstaff: mods.xstaff,
            },
        ));
    };

    let mut bar_start = 0.0f64;
    let initial_barlen = time.beats as f64 * 4.0 / time.beat_type.max(1) as f64;
    let mut barlen = initial_barlen;
    while i < toks.len() {
        let t = toks[i];
        if t == "bar" || barline_right_of(t).is_some() {
            if grace_pending {
                return Err(format!("dangling grace marker in measure {measure}"));
            }
            if let Some(style) = barline_right_of(t) {
                attr_for(&mut measure_attrs, measure + 1).barline_right = Some(style.to_string());
            }
            bar_start += barlen;
            measure += 1;
            cur_staff_idx = 0;
            voices.clear();
            voices.push(0.0);
            seg = 0;
            last_onset.clear();
            last_onset.push(None);
            for s in &mut spans {
                s.clear();
            }
            spans.truncate(1);
            if spans.is_empty() {
                spans.push(Vec::new());
            }
            grace_run = false;
            i += 1;
            continue;
        }
        if t == "<staff>" {
            if grace_pending {
                return Err(format!("dangling grace marker before <staff> in measure {measure}"));
            }
            has_explicit_staff = true;
            cur_staff_idx += 1;
            max_staff_idx = max_staff_idx.max(cur_staff_idx);
            voices.clear();
            voices.push(0.0);
            seg = 0;
            last_onset.clear();
            last_onset.push(None);
            for s in &mut spans {
                s.clear();
            }
            spans.truncate(1);
            if spans.is_empty() {
                spans.push(Vec::new());
            }
            grace_run = false;
            i += 1;
            continue;
        }
        if t == "bar-repeat-left" {
            attr_for(&mut measure_attrs, measure + 1).barline_left =
                Some("repeat-left".to_string());
            i += 1;
            continue;
        }
        if t == "volta-1" || t == "volta-2" {
            let v = if t == "volta-1" { "1" } else { "2" };
            attr_for(&mut measure_attrs, measure + 1).volta = Some(v.to_string());
            i += 1;
            continue;
        }
        if t == "grace" {
            if grace_pending {
                return Err(format!("double grace marker in measure {measure}"));
            }
            grace_pending = true;
            i += 1;
            continue;
        }
        if t.starts_with("clef-") {
            let (sign, line, oct) = parse_clef_token(t)?;
            let target_seg = resolve_target_seg(&toks, i + 1, seg);
            let onset_beats = voices.get(target_seg).copied().unwrap_or(0.0);
            let explicit_st = has_explicit_staff.then_some(cur_staff_idx);
            if !seen_event && measure == 0 {
                leading_clefs.push((sign, line, oct));
                leading_keys.push(None);
                if leading_clefs.len() <= 1 {
                    if let Some(o) = oct {
                        let a = attr_for(&mut measure_attrs, 1);
                        a.clef_sign = Some(sign);
                        a.clef_line = Some(line);
                        a.clef_octave_change = Some(o);
                    }
                }
            } else if onset_beats > 1e-6 {
                let val = match oct {
                    Some(-1) => format!("{sign}-{line}-8vb"),
                    Some(1) => format!("{sign}-{line}-8va"),
                    _ => format!("{sign}-{line}"),
                };
                let te = TimedEvent {
                    onset_beats,
                    kind: "clef".to_string(),
                    value: val,
                };
                if leading_clefs.len() <= 1 && !has_explicit_staff {
                    attr_for(&mut measure_attrs, measure + 1)
                        .timed_events
                        .push(te);
                } else {
                    seg_timed.push((measure, explicit_st, target_seg, te));
                }
            } else if leading_clefs.len() <= 1 && !has_explicit_staff {
                let a = attr_for(&mut measure_attrs, measure + 1);
                a.clef_sign = Some(sign);
                a.clef_line = Some(line);
                a.clef_octave_change = oct;
            } else {
                seg_clefs.push((measure, explicit_st, target_seg, sign, line, oct));
            }
            i += 1;
            continue;
        }
        if let Some(rest) = t.strip_prefix("key") {
            let fifths = rest
                .parse::<i32>()
                .map_err(|_| format!("bad key token {t:?}"))?;
            let target_seg = resolve_target_seg(&toks, i + 1, seg);
            let onset_beats = voices.get(target_seg).copied().unwrap_or(0.0);
            let explicit_st = has_explicit_staff.then_some(cur_staff_idx);
            if !seen_event && measure == 0 {
                if let Some(slot) = leading_keys.last_mut() {
                    *slot = Some(fifths);
                    if leading_keys.len() == 1 {
                        key_fifths = fifths;
                    }
                } else {
                    key_fifths = fifths;
                }
            } else if onset_beats > 1e-6 {
                let te = TimedEvent {
                    onset_beats,
                    kind: "key".to_string(),
                    value: fifths.to_string(),
                };
                if leading_clefs.len() <= 1 && !has_explicit_staff {
                    attr_for(&mut measure_attrs, measure + 1)
                        .timed_events
                        .push(te);
                } else {
                    seg_timed.push((measure, explicit_st, target_seg, te));
                }
            } else {
                let has_distinct_keys = {
                    let first = leading_keys.iter().flatten().next().copied();
                    first.is_some_and(|fk| leading_keys.iter().flatten().any(|&k| k != fk))
                };
                if (leading_clefs.len() > 1 || has_explicit_staff) && has_distinct_keys {
                    seg_keys.push((measure, explicit_st, target_seg, fifths));
                } else {
                    attr_for(&mut measure_attrs, measure + 1).key_fifths = Some(fifths);
                }
            }
            i += 1;
            continue;
        }
        if let Some(rest) = t.strip_prefix("time-") {
            let mut parts = rest.split('-');
            let beats: u32 = parts
                .next()
                .and_then(|s| s.parse().ok())
                .ok_or_else(|| format!("bad time token {t:?}"))?;
            let beat_type: u32 = parts
                .next()
                .and_then(|s| s.parse().ok())
                .ok_or_else(|| format!("bad time token {t:?}"))?;
            if beats == 0 || beat_type == 0 {
                return Err(format!("bad time token {t:?}"));
            }
            let target_seg = resolve_target_seg(&toks, i + 1, seg);
            let onset_beats = voices.get(target_seg).copied().unwrap_or(0.0);
            if !seen_event && measure == 0 {
                time = TimeSig { beats, beat_type };
                barlen = beats as f64 * 4.0 / beat_type.max(1) as f64;
            } else if onset_beats > 1e-6 {
                attr_for(&mut measure_attrs, measure + 1)
                    .timed_events
                    .push(TimedEvent {
                        onset_beats,
                        kind: "time".to_string(),
                        value: format!("{beats}-{beat_type}"),
                    });
            } else {
                attr_for(&mut measure_attrs, measure + 1).time = Some(TimeSig { beats, beat_type });
                barlen = beats as f64 * 4.0 / beat_type.max(1) as f64;
            }
            i += 1;
            continue;
        }
        if let Some(short) = t.strip_prefix("dyn-") {
            match short {
                "ppp" | "pp" | "p" | "mp" | "mf" | "f" | "ff" | "fff" | "fp" | "sf" | "sfp"
                | "sfz" | "fz" | "rfz" | "sffz" => {
                    let target_seg = resolve_target_seg(&toks, i + 1, seg);
                    let onset_beats = voices.get(target_seg).copied().unwrap_or(0.0);
                    let explicit_st = has_explicit_staff.then_some(cur_staff_idx);
                    if leading_clefs.len() <= 1 && !has_explicit_staff {
                        let a = attr_for(&mut measure_attrs, measure + 1);
                        a.push_dynamic(short);
                        a.timed_events.push(TimedEvent {
                            onset_beats,
                            kind: "dynamic".to_string(),
                            value: short.to_string(),
                        });
                    } else {
                        seg_dyns.push((measure, explicit_st, target_seg, onset_beats, short.to_string()));
                    }
                }
                _ => return Err(format!("unexpected token {t:?} in measure {measure}")),
            }
            i += 1;
            continue;
        }
        if let Some(short) = t.strip_prefix("hairpin-") {
            let hp = match short {
                "cresc-start" => "cresc",
                "dim-start" => "dim",
                "stop" => "stop",
                _ => return Err(format!("unexpected token {t:?} in measure {measure}")),
            };
            let target_seg = resolve_target_seg(&toks, i + 1, seg);
            let onset_beats = voices.get(target_seg).copied().unwrap_or(0.0);
            let explicit_st = has_explicit_staff.then_some(cur_staff_idx);
            if leading_clefs.len() <= 1 && !has_explicit_staff {
                let a = attr_for(&mut measure_attrs, measure + 1);
                a.push_hairpin(hp);
                a.timed_events.push(TimedEvent {
                    onset_beats,
                    kind: "hairpin".to_string(),
                    value: hp.to_string(),
                });
            } else {
                seg_hairpins.push((measure, explicit_st, target_seg, onset_beats, hp.to_string()));
            }
            i += 1;
            continue;
        }
        if let Some(short) = t.strip_prefix("ottava-") {
            let val = match short {
                "8va" => "8va",
                "8vb" => "8vb",
                "stop" => "stop",
                _ => return Err(format!("unexpected token {t:?} in measure {measure}")),
            };
            let target_seg = resolve_target_seg(&toks, i + 1, seg);
            let onset_beats = voices.get(target_seg).copied().unwrap_or(0.0);
            let explicit_st = has_explicit_staff.then_some(cur_staff_idx);
            let te = TimedEvent {
                onset_beats,
                kind: "ottava".to_string(),
                value: val.to_string(),
            };
            if leading_clefs.len() <= 1 && !has_explicit_staff {
                attr_for(&mut measure_attrs, measure + 1)
                    .timed_events
                    .push(te);
            } else {
                seg_timed.push((measure, explicit_st, target_seg, te));
            }
            i += 1;
            continue;
        }
        if let Some(rest) = t.strip_prefix("voice-") {
            let n: usize = rest.parse().map_err(|_| format!("bad voice token {t:?}"))?;
            if n == 0 {
                return Err(format!("bad voice token {t:?}"));
            }
            while voices.len() < n {
                voices.push(0.0);
                last_onset.push(None);
                spans.push(Vec::new());
            }
            seg = n - 1;
            i += 1;
            continue;
        }
        if let Some(rest) = t.strip_prefix("tuplet-start-") {
            let mut parts = rest.split('-');
            let a: u32 = parts
                .next()
                .and_then(|s| s.parse().ok())
                .ok_or_else(|| format!("bad tuplet token {t:?}"))?;
            let n: u32 = parts
                .next()
                .and_then(|s| s.parse().ok())
                .ok_or_else(|| format!("bad tuplet token {t:?}"))?;
            if a == 0 || n == 0 {
                return Err(format!("bad tuplet token {t:?}"));
            }
            pending_spans.push((a, n));
            i += 1;
            continue;
        }
        if t == "tup-open" {
            let a = toks
                .get(i + 1)
                .and_then(|s| s.parse::<u32>().ok())
                .filter(|&v| v > 0);
            let n = toks
                .get(i + 2)
                .and_then(|s| s.parse::<u32>().ok())
                .filter(|&v| v > 0);
            if let (Some(a), Some(n)) = (a, n) {
                pending_spans.push((a, n));
                i += 3;
            } else {
                return Err(format!("bad tup-open in measure {measure}: expected A N"));
            }
            continue;
        }
        if t == "tuplet-stop" || t == "tup-close" {
            spans[seg]
                .pop()
                .ok_or_else(|| format!("unmatched {t} in measure {measure}"))?;
            i += 1;
            continue;
        }
        // Note or rest event: [grace] [chord] (dur | rest [dur]) ...
        let mut chord = false;
        if t == "chord" {
            if grace_pending {
                return Err(format!("grace without duration in measure {measure}"));
            }
            chord = true;
            i += 1;
        }
        seen_event = true;
        // Event-level grace: the `grace` marker is consumed by the
        // duration token (chord tones inherit the run's grace).
        let is_grace = if chord {
            grace_run
        } else {
            let g = grace_pending;
            grace_pending = false;
            grace_run = g;
            g
        };
        let t = toks
            .get(i)
            .ok_or_else(|| format!("dangling chord marker in measure {measure}"))?;
        let (pitched, written) = if *t == "rest" {
            i += 1;
            (false, 0.0)
        } else if let Some(w) = written_dur(t) {
            i += 1;
            (true, w)
        } else if *t == "bar-rest" {
            if is_grace {
                return Err(format!("grace on bar-rest in measure {measure}"));
            }
            // Whole-bar rest: emit with full-bar duration (unscored),
            // jump this voice to the bar end.
            let onset = bar_start + voices[seg];
            push_rest(
                &mut tagged_notes,
                measure,
                cur_staff_idx,
                onset,
                barlen,
                seg as u32 + 1,
                &NoteMods::default(),
                false,
                None,
            );
            bar_rest_count += 1;
            voices[seg] = barlen;
            last_onset[seg] = None;
            spans[seg].append(&mut pending_spans);
            i += 1;
            continue;
        } else {
            return Err(format!("unexpected token {t:?} in measure {measure}"));
        };
        // Step + octave follow the duration token (alter, dots, ties
        // after — see below).
        let (step, octave) = if pitched {
            let s = toks
                .get(i)
                .ok_or_else(|| format!("missing step in measure {measure}"))?;
            let step = s
                .chars()
                .next()
                .ok_or_else(|| format!("bad step token {s:?} in measure {measure}"))?;
            if !"ABCDEFG".contains(step) {
                return Err(format!("bad step token {s:?} in measure {measure}"));
            }
            i += 1;
            let o = toks
                .get(i)
                .ok_or_else(|| format!("missing octave in measure {measure}"))?;
            let octave: i32 = o
                .parse()
                .map_err(|_| format!("bad octave token {o:?} in measure {measure}"))?;
            i += 1;
            (Some(step), Some(octave))
        } else {
            (None, None)
        };
        // Encoder order: step octave [alter] [dots] [slur-start]
        // [slur-stop] [modifiers] [tie-start] [tie-stop]; plain rests:
        // `rest` duration [dots] [modifiers]. The loops below accept any
        // interleave a generator might emit (the grammar mask agrees).
        let mut alter = 0i32;
        if pitched {
            // Alter tokens (`a-1`, `a1`, …) share their leading `a`
            // with the `accent` modifier: only consume the token here
            // when the suffix parses as an accidental offset, so a
            // following modifier falls through to the tail loop.
            if let Some(a) = toks
                .get(i)
                .filter(|s| s.starts_with('a') && s[1..].parse::<i32>().is_ok())
            {
                alter = a[1..]
                    .parse()
                    .map_err(|_| format!("bad alter token {a:?} in measure {measure}"))?;
                i += 1;
            }
        }
        let mut dots = 0u32;
        while toks.get(i) == Some(&"dot") {
            dots += 1;
            i += 1;
        }
        let mut dur_written = written * (2.0 - 2f64.powi(-(dots as i32)));
        // Slur/modifier marks and tie marks in any interleave (encoder
        // order is slurs, modifiers, then ties; the loop agrees with
        // the grammar mask, which admits the same set).
        let mut mods = NoteMods::default();
        let mut tie_start = false;
        let mut tie_stop = false;
        take_tail(&toks, &mut i, &mut mods, &mut tie_start, &mut tie_stop);
        if !pitched {
            // Plain-rest duration token follows the `rest` marker.
            let d = toks
                .get(i)
                .ok_or_else(|| format!("missing rest duration in measure {measure}"))?;
            dur_written = written_dur(d)
                .ok_or_else(|| format!("bad rest duration {d:?} in measure {measure}"))?;
            i += 1;
            while toks.get(i) == Some(&"dot") {
                dots += 1;
                i += 1;
            }
            dur_written *= 2.0 - 2f64.powi(-(dots as i32));
            // Rests take no ties (a tie mark here ends the tail and
            // errors as an unexpected bound token, like the encoder).
            take_tail_rest(&toks, &mut i, &mut mods);
        }
        // Pending spans open on this event's segment (`voice-N`
        // precedes it in the stream, so `seg` is already current).
        spans[seg].append(&mut pending_spans);
        let scale = scale_of(&spans, seg);
        let cur_tuplet = ratio_of(&spans, seg);
        let performed = dur_written * scale;
        let base = bar_start;
        if chord {
            let onset = last_onset[seg]
                .ok_or_else(|| format!("dangling chord marker in measure {measure}"))?;
            let pitch = format!(
                "{}{}{}",
                step.ok_or_else(|| format!("chord on rest in measure {measure}"))?,
                alter_suffix(alter)?,
                octave.unwrap_or(4)
            );
            let key = (step.unwrap(), octave.unwrap_or(4), alter);
            if merge_ties {
                if !is_grace {
                    if let Some((sm, sst, note)) = tie_note(
                        &mut pending_tie,
                        key,
                        pitch,
                        onset,
                        performed,
                        seg as u32 + 1,
                        measure,
                        cur_staff_idx,
                        seg,
                        tie_start,
                        tie_stop,
                        &mods,
                        cur_tuplet,
                    ) {
                        tagged_notes.push((sm, sst, note));
                    }
                }
            } else {
                let mut note = mk_note(pitch, onset, performed, seg as u32 + 1, &mods, cur_tuplet);
                note.tie_start = tie_start;
                note.tie_stop = tie_stop;
                if is_grace {
                    note.grace = true;
                    note.dur = dur_written;
                    note.tuplet = None;
                }
                tagged_notes.push((measure, cur_staff_idx, note));
            }
            // Chord tones never advance the cursor.
        } else {
            let onset = base + voices[seg];
            if pitched {
                let pitch = format!(
                    "{}{}{}",
                    step.unwrap(),
                    alter_suffix(alter)?,
                    octave.unwrap_or(4)
                );
                let key = (step.unwrap(), octave.unwrap_or(4), alter);
                if merge_ties {
                    if is_grace {
                        last_onset[seg] = Some(onset);
                    } else {
                        if let Some((sm, sst, note)) = tie_note(
                            &mut pending_tie,
                            key,
                            pitch,
                            onset,
                            performed,
                            seg as u32 + 1,
                            measure,
                            cur_staff_idx,
                            seg,
                            tie_start,
                            tie_stop,
                            &mods,
                            cur_tuplet,
                        ) {
                            tagged_notes.push((sm, sst, note));
                        }
                        last_onset[seg] = Some(onset);
                    }
                } else {
                    let mut note =
                        mk_note(pitch, onset, performed, seg as u32 + 1, &mods, cur_tuplet);
                    note.tie_start = tie_start;
                    note.tie_stop = tie_stop;
                    if is_grace {
                        note.grace = true;
                        note.dur = dur_written;
                        note.tuplet = None;
                    }
                    tagged_notes.push((measure, cur_staff_idx, note));
                    last_onset[seg] = Some(onset);
                }
            } else if merge_ties && is_grace {
                last_onset[seg] = Some(onset);
            } else {
                push_rest(
                    &mut tagged_notes,
                    measure,
                    cur_staff_idx,
                    onset,
                    if is_grace { dur_written } else { performed },
                    seg as u32 + 1,
                    &mods,
                    is_grace,
                    if is_grace { None } else { cur_tuplet },
                );
                last_onset[seg] = if is_grace { Some(onset) } else { None };
            }
            // Grace notes steal no time: the cursor holds.
            if !is_grace {
                voices[seg] += performed;
            }
        }
    }
    if grace_pending {
        return Err("dangling grace marker at end of stream".to_string());
    }
    for ((s, o, a), stack) in pending_tie {
        for (sm, sst, fseg, onset, acc, fmods, ftup) in stack {
            let pitch = format!("{s}{}{}", alter_suffix(a)?, o);
            let note = mk_note(pitch, onset, acc, fseg as u32 + 1, &fmods, ftup);
            tagged_notes.push((sm, sst, note));
        }
    }
    let has_pitched = tagged_notes
        .iter()
        .any(|(_, _, n)| !n.rest && n.pitch.is_some());
    if !has_pitched && (tagged_notes.is_empty() || bar_rest_count == tagged_notes.len()) {
        tagged_notes.clear();
        measure_attrs.clear();
        seg_clefs.clear();
        seg_keys.clear();
        seg_dyns.clear();
        seg_hairpins.clear();
        seg_timed.clear();
    }

    measure_attrs.sort_by_key(|a| a.measure);

    let parts = if leading_clefs.len() <= 1 && !has_explicit_staff {
        let (clef_sign, clef_line, clef_octave_change) = match leading_clefs.first() {
            Some(&(s, l, o)) => (Some(s), Some(l), o),
            None => (None, None, None),
        };
        vec![PartInput {
            name: "decoded".to_string(),
            notes: tagged_notes.into_iter().map(|(_, _, n)| n).collect(),
            clef_sign,
            clef_line,
            clef_octave_change,
            key_fifths: leading_keys.first().copied().flatten(),
            measure_attrs: Vec::new(),
        }]
    } else if has_explicit_staff {
        let k = leading_clefs.len().max(max_staff_idx + 1).max(1);
        let mut clefs = leading_clefs.clone();
        while clefs.len() < k {
            clefs.push(if clefs.len() == 1 {
                ('F', 4, None)
            } else {
                ('G', 2, None)
            });
        }
        let total_notes = tagged_notes.len();
        let mut part_notes: Vec<Vec<NoteInput>> = vec![Vec::new(); k];
        for (_m, st_idx, n) in tagged_notes {
            let st = st_idx.min(k - 1);
            part_notes[st].push(n);
        }
        let mut part_measure_attrs: Vec<Vec<MeasureAttr>> = vec![Vec::new(); k];
        for (m, explicit_st, _s_idx, sign, line, oct) in seg_clefs {
            let st = explicit_st.unwrap_or(0).min(k - 1);
            let a = attr_for(&mut part_measure_attrs[st], m + 1);
            a.clef_sign = Some(sign);
            a.clef_line = Some(line);
            a.clef_octave_change = oct;
        }
        for (m, explicit_st, _s_idx, fifths) in seg_keys {
            let st = explicit_st.unwrap_or(0).min(k - 1);
            attr_for(&mut part_measure_attrs[st], m + 1).key_fifths = Some(fifths);
        }
        for (m, explicit_st, _s_idx, onset_beats, d) in seg_dyns {
            let st = explicit_st.unwrap_or(0).min(k - 1);
            let a = attr_for(&mut part_measure_attrs[st], m + 1);
            a.push_dynamic(&d);
            a.timed_events.push(TimedEvent {
                onset_beats,
                kind: "dynamic".to_string(),
                value: d,
            });
        }
        for (m, explicit_st, _s_idx, onset_beats, h) in seg_hairpins {
            let st = explicit_st.unwrap_or(0).min(k - 1);
            let a = attr_for(&mut part_measure_attrs[st], m + 1);
            a.push_hairpin(&h);
            a.timed_events.push(TimedEvent {
                onset_beats,
                kind: "hairpin".to_string(),
                value: h,
            });
        }
        for (m, explicit_st, _s_idx, te) in seg_timed {
            let st = explicit_st.unwrap_or(0).min(k - 1);
            attr_for(&mut part_measure_attrs[st], m + 1)
                .timed_events
                .push(te);
        }
        for attrs in &mut part_measure_attrs {
            attrs.sort_by_key(|a| a.measure);
        }
        if total_notes > 0 {
            for pnotes in &mut part_notes {
                if pnotes.is_empty() {
                    pnotes.push(NoteInput::rest(0.0, initial_barlen.max(1.0)));
                }
            }
        }
        clefs
            .iter()
            .zip(part_notes)
            .zip(part_measure_attrs)
            .enumerate()
            .map(|(idx, ((&(s, l, o), notes), p_attrs))| PartInput {
                name: format!("Staff {}", idx + 1),
                notes,
                clef_sign: Some(s),
                clef_line: Some(l),
                clef_octave_change: o,
                key_fifths: leading_keys.get(idx).copied().flatten(),
                measure_attrs: p_attrs,
            })
            .collect()
    } else {
        let k = leading_clefs.len();
        // Per-measure voice-segment pitch statistics for monotonic staff assignment.
        let mut by_measure: std::collections::BTreeMap<u32, Vec<(f64, usize)>> =
            std::collections::BTreeMap::new();
        for (m, _st, n) in &tagged_notes {
            let seg_idx = n.voice.saturating_sub(1) as usize;
            let entry = by_measure.entry(*m).or_default();
            if entry.len() <= seg_idx {
                entry.resize(seg_idx + 1, (0.0, 0));
            }
            if !n.rest {
                if let Some(p_str) = n.pitch.as_deref() {
                    if let Ok(parsed) = crate::pitch::parse_pitch(p_str) {
                        let midi =
                            crate::pitch::pitch_to_midi(parsed.step, parsed.alter, parsed.octave);
                        entry[seg_idx].0 += midi as f64;
                        entry[seg_idx].1 += 1;
                    }
                }
            }
        }
        let mut measure_maps: std::collections::BTreeMap<u32, Vec<(usize, u32)>> =
            std::collections::BTreeMap::new();
        let mut running_staff_midi: Vec<Option<f64>> = vec![None; k];
        for (m, stats) in by_measure {
            let means: Vec<Option<f64>> = stats
                .iter()
                .map(|&(sum, cnt)| if cnt > 0 { Some(sum / cnt as f64) } else { None })
                .collect();
            let map = assign_segments_to_staves(&means, &leading_clefs, &running_staff_midi);
            // Update per-staff running mean MIDI for cross-measure continuity.
            let mut staff_acc = vec![(0.0f64, 0usize); k];
            for (seg_idx, &(st, _)) in map.iter().enumerate() {
                if st < k && seg_idx < stats.len() {
                    staff_acc[st].0 += stats[seg_idx].0;
                    staff_acc[st].1 += stats[seg_idx].1;
                }
            }
            for st in 0..k {
                if staff_acc[st].1 > 0 {
                    let cur_mean = staff_acc[st].0 / staff_acc[st].1 as f64;
                    running_staff_midi[st] = Some(match running_staff_midi[st] {
                        Some(prev) => 0.7 * prev + 0.3 * cur_mean,
                        None => cur_mean,
                    });
                }
            }
            measure_maps.insert(m, map);
        }

        let total_notes = tagged_notes.len();
        let mut part_notes: Vec<Vec<NoteInput>> = vec![Vec::new(); k];
        for (m, _st, mut n) in tagged_notes {
            let seg_idx = n.voice.saturating_sub(1) as usize;
            let (staff_idx, local_voice) = measure_maps
                .get(&m)
                .and_then(|map| map.get(seg_idx).copied())
                .unwrap_or((seg_idx.min(k - 1), 1));
            n.voice = local_voice;
            part_notes[staff_idx].push(n);
        }

        let mut part_measure_attrs: Vec<Vec<MeasureAttr>> = vec![Vec::new(); k];
        let staff_of = |m: u32, seg_idx: usize| -> usize {
            measure_maps
                .get(&m)
                .and_then(|map| map.get(seg_idx).copied())
                .map(|(st, _)| st)
                .unwrap_or_else(|| seg_idx.min(k - 1))
        };
        for (m, _est, s_idx, sign, line, oct) in seg_clefs {
            let st = staff_of(m, s_idx);
            let a = attr_for(&mut part_measure_attrs[st], m + 1);
            a.clef_sign = Some(sign);
            a.clef_line = Some(line);
            a.clef_octave_change = oct;
        }
        for (m, _est, s_idx, fifths) in seg_keys {
            let st = staff_of(m, s_idx);
            attr_for(&mut part_measure_attrs[st], m + 1).key_fifths = Some(fifths);
        }
        for (m, _est, s_idx, onset_beats, d) in seg_dyns {
            let st = staff_of(m, s_idx);
            let a = attr_for(&mut part_measure_attrs[st], m + 1);
            a.push_dynamic(&d);
            a.timed_events.push(TimedEvent {
                onset_beats,
                kind: "dynamic".to_string(),
                value: d,
            });
        }
        for (m, _est, s_idx, onset_beats, h) in seg_hairpins {
            let st = staff_of(m, s_idx);
            let a = attr_for(&mut part_measure_attrs[st], m + 1);
            a.push_hairpin(&h);
            a.timed_events.push(TimedEvent {
                onset_beats,
                kind: "hairpin".to_string(),
                value: h,
            });
        }
        for (m, _est, s_idx, te) in seg_timed {
            let st = staff_of(m, s_idx);
            attr_for(&mut part_measure_attrs[st], m + 1)
                .timed_events
                .push(te);
        }
        for attrs in &mut part_measure_attrs {
            attrs.sort_by_key(|a| a.measure);
        }

        if total_notes > 0 {
            for pnotes in &mut part_notes {
                if pnotes.is_empty() {
                    pnotes.push(NoteInput::rest(0.0, initial_barlen.max(1.0)));
                }
            }
        }

        leading_clefs
            .iter()
            .zip(part_notes)
            .zip(part_measure_attrs)
            .enumerate()
            .map(|(idx, ((&(s, l, o), notes), p_attrs))| PartInput {
                name: format!("Staff {}", idx + 1),
                notes,
                clef_sign: Some(s),
                clef_line: Some(l),
                clef_octave_change: o,
                key_fifths: leading_keys.get(idx).copied().flatten(),
                measure_attrs: p_attrs,
            })
            .collect()
    };

    Ok(ScoreInput {
        title: None,
        composer: None,
        divisions: SCORE_DIVISIONS,
        time: Some(time),
        key_fifths,
        parts,
        notes: Vec::new(),
        part_name: None,
        measure_attrs,
    })
}

#[cfg(test)]
mod tests {
    use super::decode_bekern;

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn roundtrip_ds0001_page_one_is_note_exact() {
        // The load-bearing invariant: encode->decode inverts exactly on
        // real multi-voice piano texture (chords, ties, bar rests,
        // 6 voice segments). Pitched notes compared as (pitch,
        // onset, dur) quantized to ticks; voice numbering differs by
        // design (appearance order vs MusicXML numbers) and rests are
        // unscored, so neither is compared.
        use super::super::bekern::to_bekern;
        use super::super::musicxml_import::{
            bekern_events, page_breaks, score_inputs_by_page, staff_headers,
        };
        let xml = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/ds0001.musicxml"
        ))
        .expect("corpus fixture ds0001.musicxml");
        let pages = score_inputs_by_page(&xml).unwrap();
        let gt = &pages[0].input;
        let breaks = page_breaks(&xml);
        let start = breaks[0].1;
        let end = breaks.get(1).map(|(_, m)| m - 1).unwrap_or(u32::MAX);
        let events: Vec<_> = bekern_events(&xml)
            .unwrap()
            .into_iter()
            .filter(|e| e.measure >= start && e.measure <= end)
            .collect();
        let headers = staff_headers(&xml).unwrap();
        let (toks, _) = to_bekern(&events, &headers).unwrap();
        let hyp = decode_bekern(&toks.join(" ")).unwrap();
        fn pitched(input: &super::super::ScoreInput) -> Vec<(String, i64, i64)> {
            let mut out = Vec::new();
            for part in &input.parts {
                for n in &part.notes {
                    if n.rest || n.pitch.is_none() {
                        continue;
                    }
                    out.push((
                        n.pitch.clone().unwrap(),
                        (n.onset * 480.0).round() as i64,
                        (n.dur * 480.0).round() as i64,
                    ));
                }
            }
            out.sort();
            out
        }
        let g = pitched(gt);
        let h = pitched(&hyp);
        assert_eq!(g.len(), h.len(), "note count survives the round trip");
        assert_eq!(g, h, "every pitched note survives the round trip");
    }

    #[test]
    fn roundtrip_ds0409_cross_bar_tuplets_are_note_exact() {
        // Regression: to_bekern used to emit tuplet spans crossing
        // `bar`, which decode's per-measure reset could not invert
        // (341/2529 corpus pages failed with "unmatched tuplet-stop"
        // or an index panic). Spans are now split at barlines on encode
        // and tracked per segment on decode: every ds0409 page (the
        // cross-bar case) must round-trip exactly.
        use super::super::bekern::to_bekern;
        use super::super::musicxml_import::{
            bekern_events, page_breaks, score_inputs_by_page, staff_headers,
        };
        let xml = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/ds0409.musicxml"
        ))
        .expect("corpus fixture ds0409.musicxml");
        let pages = score_inputs_by_page(&xml).unwrap();
        let breaks = page_breaks(&xml);
        assert_eq!(pages.len(), breaks.len(), "pages align with breaks");
        let headers = staff_headers(&xml).unwrap();
        let all = bekern_events(&xml).unwrap();
        // Tie chains severed by pagination cannot round-trip exactly:
        // the whole-score native path merges the chain, per-page tokens
        // sever it (decode flushes/partials deterministically). A page
        // is tie-clean iff every in-page tie_start's chain ends in-page
        // and every in-page tie_stop's chain starts in-page.
        fn tie_clean(
            all: &[super::super::musicxml_import::BekernNote],
            start: u32,
            end: u32,
        ) -> bool {
            let key = |e: &super::super::musicxml_import::BekernNote| {
                e.pitch.map(|(s, o, a)| (e.staff, e.voice, s, o, a))
            };
            let in_page = |m: u32| m >= start && m <= end;
            for (i, e) in all.iter().enumerate() {
                if !in_page(e.measure) {
                    continue;
                }
                let k = match key(e) {
                    Some(k) => k,
                    None => continue,
                };
                if e.tie_start {
                    // Walk the chain forward; every link must be in-page.
                    let mut j = i;
                    loop {
                        let nxt = all[j + 1..]
                            .iter()
                            .position(|o| key(o) == Some(k))
                            .map(|p| j + 1 + p);
                        match nxt {
                            Some(n) if in_page(all[n].measure) => {
                                if !all[n].tie_start {
                                    break;
                                }
                                j = n;
                            }
                            _ => return false,
                        }
                    }
                }
                if e.tie_stop {
                    // Walk the chain backward; every link must be in-page.
                    let mut j = i;
                    loop {
                        let prv = all[..j].iter().rposition(|o| key(o) == Some(k));
                        match prv {
                            Some(n) if in_page(all[n].measure) => {
                                if !all[n].tie_stop {
                                    break;
                                }
                                j = n;
                            }
                            _ => return false,
                        }
                    }
                }
            }
            true
        }
        // (pitch, global-measure-0idx, onset-in-bar, dur): measure is
        // comparable across sides when bar counting is right; a bar bug
        // scatters diffs everywhere, a local cause clusters them.
        fn pitched(
            input: &super::super::ScoreInput,
            barlen_ticks: i64,
            base_ticks: i64,
        ) -> Vec<(String, i64, i64, i64)> {
            let mut out = Vec::new();
            for part in &input.parts {
                for n in &part.notes {
                    if n.rest || n.pitch.is_none() {
                        continue;
                    }
                    // Both spellings are input-legal (README) and the
                    // scorer is MIDI-based; unify decode's "x" with the
                    // import path's "##" before comparing strings.
                    let abs = (n.onset * 480.0).round() as i64 + base_ticks;
                    out.push((
                        n.pitch.clone().unwrap().replace('x', "##"),
                        abs / barlen_ticks,
                        abs % barlen_ticks,
                        (n.dur * 480.0).round() as i64,
                    ));
                }
            }
            out.sort();
            out
        }
        let mut exact_pages = 0u32;
        for (pi, page) in pages.iter().enumerate() {
            let start = breaks[pi].1;
            let end = breaks.get(pi + 1).map(|(_, m)| m - 1).unwrap_or(u32::MAX);
            let events: Vec<_> = all
                .iter()
                .filter(|e| e.measure >= start && e.measure <= end)
                .collect();
            // to_bekern takes &[BekernNote]; re-collect owned refs.
            let owned: Vec<_> = events.into_iter().cloned().collect();
            let (toks, _) = to_bekern(&owned, &headers).unwrap();
            let hyp = decode_bekern(&toks.join(" "))
                .unwrap_or_else(|e| panic!("ds0409 page {pi} decodes: {e}"));
            if !tie_clean(&all, start, end) {
                // Severed tie chains: decode parses (pinned above) but
                // whole-score native merging cannot match per-page
                // tokens by construction. Exactness is pinned on
                // tie-clean pages below + the ds0001 page test.
                continue;
            }
            // Page 1+ starts mid-score: GT onsets are absolute beats
            // while decode counts measures from the page start, so the
            // comparison below is onset-in-bar (the strict scorer's own
            // notion) + duration.
            let barlen_ticks = {
                let t = page.input.time.unwrap_or(super::super::TimeSig {
                    beats: 4,
                    beat_type: 4,
                });
                (t.beats as f64 * 4.0 / t.beat_type.max(1) as f64 * 480.0).round() as i64
            };
            let want = pitched(&page.input, barlen_ticks, 0);
            let got = pitched(&hyp, barlen_ticks, (start as i64 - 1) * barlen_ticks);
            if want != got {
                // Per-measure diff histogram (global 1-indexed
                // measures): clusters = local cause, scatter = bar bug.
                use std::collections::BTreeMap;
                let mut wmap: BTreeMap<i64, Vec<_>> = BTreeMap::new();
                for n in want.iter() {
                    wmap.entry(n.1 + 1).or_default().push(n.clone());
                }
                let mut gmap: BTreeMap<i64, Vec<_>> = BTreeMap::new();
                for n in got.iter() {
                    gmap.entry(n.1 + 1).or_default().push(n.clone());
                }
                for (m, w) in wmap.iter() {
                    let g = gmap.get(m).cloned().unwrap_or_default();
                    let (mut w, mut g) = (w.clone(), g);
                    w.sort();
                    g.sort();
                    if w != g {
                        eprintln!(
                            "page {pi} measure {m}: want {} notes, got {}",
                            w.len(),
                            g.len()
                        );
                    }
                }
            }
            assert_eq!(want, got, "ds0409 page {pi} survives the round trip");
            exact_pages += 1;
        }
        assert!(
            exact_pages > 0,
            "at least one ds0409 page must round-trip exactly"
        );
    }

    #[test]
    fn roundtrip_ds0037_nested_tuplets_are_note_exact() {
        // Diagnostic for nested tuplet brackets sharing one note: ds0037
        // measure 27 opens brackets 1+2 on one A and closes both on one
        // Bb (last-wins keeps number 2). If the dropped number-1 pair is
        // truly redundant, every tie-clean page round-trips exactly; if
        // not, the diff histogram names the damaged measures.
        use super::super::bekern::to_bekern;
        use super::super::musicxml_import::{
            bekern_events, page_breaks, score_inputs_by_page, staff_headers,
        };
        let xml = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/ds0037.musicxml"
        ))
        .expect("corpus fixture ds0037.musicxml");
        let pages = score_inputs_by_page(&xml).unwrap();
        let breaks = page_breaks(&xml);
        assert_eq!(pages.len(), breaks.len(), "pages align with breaks");
        let headers = staff_headers(&xml).unwrap();
        let all = bekern_events(&xml).unwrap();
        // Tie chains severed by pagination cannot round-trip exactly:
        // the whole-score native path merges the chain, per-page tokens
        // sever it (decode flushes/partials deterministically). A page
        // is tie-clean iff every in-page tie_start's chain ends in-page
        // and every in-page tie_stop's chain starts in-page.
        fn tie_clean(
            all: &[super::super::musicxml_import::BekernNote],
            start: u32,
            end: u32,
        ) -> bool {
            let key = |e: &super::super::musicxml_import::BekernNote| {
                e.pitch.map(|(s, o, a)| (e.staff, e.voice, s, o, a))
            };
            let in_page = |m: u32| m >= start && m <= end;
            for (i, e) in all.iter().enumerate() {
                if !in_page(e.measure) {
                    continue;
                }
                let k = match key(e) {
                    Some(k) => k,
                    None => continue,
                };
                if e.tie_start {
                    // Walk the chain forward; every link must be in-page.
                    let mut j = i;
                    loop {
                        let nxt = all[j + 1..]
                            .iter()
                            .position(|o| key(o) == Some(k))
                            .map(|p| j + 1 + p);
                        match nxt {
                            Some(n) if in_page(all[n].measure) => {
                                if !all[n].tie_start {
                                    break;
                                }
                                j = n;
                            }
                            _ => return false,
                        }
                    }
                }
                if e.tie_stop {
                    // Walk the chain backward; every link must be in-page.
                    let mut j = i;
                    loop {
                        let prv = all[..j].iter().rposition(|o| key(o) == Some(k));
                        match prv {
                            Some(n) if in_page(all[n].measure) => {
                                if !all[n].tie_stop {
                                    break;
                                }
                                j = n;
                            }
                            _ => return false,
                        }
                    }
                }
            }
            true
        }
        // (pitch, global-measure-0idx, onset-in-bar, dur): measure is
        // comparable across sides when bar counting is right; a bar bug
        // scatters diffs everywhere, a local cause clusters them.
        fn pitched(
            input: &super::super::ScoreInput,
            barlen_ticks: i64,
            base_ticks: i64,
        ) -> Vec<(String, i64, i64, i64)> {
            let mut out = Vec::new();
            for part in &input.parts {
                for n in &part.notes {
                    if n.rest || n.pitch.is_none() {
                        continue;
                    }
                    // Both spellings are input-legal (README) and the
                    // scorer is MIDI-based; unify decode's "x" with the
                    // import path's "##" before comparing strings.
                    let abs = (n.onset * 480.0).round() as i64 + base_ticks;
                    out.push((
                        n.pitch.clone().unwrap().replace('x', "##"),
                        abs / barlen_ticks,
                        abs % barlen_ticks,
                        (n.dur * 480.0).round() as i64,
                    ));
                }
            }
            out.sort();
            out
        }
        let mut exact_pages = 0u32;
        for (pi, page) in pages.iter().enumerate() {
            let start = breaks[pi].1;
            let end = breaks.get(pi + 1).map(|(_, m)| m - 1).unwrap_or(u32::MAX);
            let events: Vec<_> = all
                .iter()
                .filter(|e| e.measure >= start && e.measure <= end)
                .collect();
            // to_bekern takes &[BekernNote]; re-collect owned refs.
            let owned: Vec<_> = events.into_iter().cloned().collect();
            let (toks, _) = to_bekern(&owned, &headers).unwrap();
            let hyp = decode_bekern(&toks.join(" "))
                .unwrap_or_else(|e| panic!("ds0037 page {pi} decodes: {e}"));
            if !tie_clean(&all, start, end) {
                // Severed tie chains: decode parses (pinned above) but
                // whole-score native merging cannot match per-page
                // tokens by construction. Exactness is pinned on
                // tie-clean pages below + the ds0001 page test.
                continue;
            }
            // Page 1+ starts mid-score: GT onsets are absolute beats
            // while decode counts measures from the page start, so the
            // comparison below is onset-in-bar (the strict scorer's own
            // notion) + duration.
            let barlen_ticks = {
                let t = page.input.time.unwrap_or(super::super::TimeSig {
                    beats: 4,
                    beat_type: 4,
                });
                (t.beats as f64 * 4.0 / t.beat_type.max(1) as f64 * 480.0).round() as i64
            };
            let want = pitched(&page.input, barlen_ticks, 0);
            let got = pitched(&hyp, barlen_ticks, (start as i64 - 1) * barlen_ticks);
            if want != got {
                // Per-measure diff histogram (global 1-indexed
                // measures): clusters = local cause, scatter = bar bug.
                use std::collections::BTreeMap;
                let mut wmap: BTreeMap<i64, Vec<_>> = BTreeMap::new();
                for n in want.iter() {
                    wmap.entry(n.1 + 1).or_default().push(n.clone());
                }
                let mut gmap: BTreeMap<i64, Vec<_>> = BTreeMap::new();
                for n in got.iter() {
                    gmap.entry(n.1 + 1).or_default().push(n.clone());
                }
                for (m, w) in wmap.iter() {
                    let g = gmap.get(m).cloned().unwrap_or_default();
                    let (mut w, mut g) = (w.clone(), g);
                    w.sort();
                    g.sort();
                    if w != g {
                        eprintln!(
                            "page {pi} measure {m}: want {} notes, got {}",
                            w.len(),
                            g.len()
                        );
                        // Multiset diff, capped: the actual damaged notes.
                        let mut gw = g.clone();
                        let mut missing = Vec::new();
                        for n in w.iter() {
                            if let Some(p) = gw.iter().position(|x| x == n) {
                                gw.remove(p);
                            } else {
                                missing.push(n.clone());
                            }
                        }
                        let mut ww = w.clone();
                        let mut extra = Vec::new();
                        for n in g.iter() {
                            if let Some(p) = ww.iter().position(|x| x == n) {
                                ww.remove(p);
                            } else {
                                extra.push(n.clone());
                            }
                        }
                        for n in missing.iter().take(8) {
                            eprintln!("  want-not-got {n:?}");
                        }
                        for n in extra.iter().take(8) {
                            eprintln!("  got-not-want {n:?}");
                        }
                    }
                }
            }
            assert_eq!(want, got, "ds0037 page {pi} survives the round trip");
            exact_pages += 1;
        }
        assert!(
            exact_pages > 0,
            "at least one ds0037 page must round-trip exactly"
        );
    }

    // Parallel tied chord tones (both members tied across the same
    // boundary): each chain merges at its own start. A single pending
    // slot would drop the first chain and split the second.
    const PARALLEL_TIES: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<score-partwise version="3.1">
<part-list><score-part id="P1"><part-name>Voice</part-name></score-part></part-list>
<part id="P1">
<measure number="1">
<attributes><divisions>4</divisions><clef><sign>G</sign><line>2</line></clef><key><fifths>0</fifths></key><time><beats>4</beats><beat-type>4</beat-type></time></attributes>
<note><pitch><step>C</step><octave>4</octave></pitch><duration>4</duration><voice>1</voice><notations><tied type="start"/></notations></note>
<note><chord/><pitch><step>E</step><octave>4</octave></pitch><duration>4</duration><voice>1</voice><notations><tied type="start"/></notations></note>
<note><pitch><step>C</step><octave>4</octave></pitch><duration>4</duration><voice>1</voice><notations><tied type="stop"/></notations></note>
<note><chord/><pitch><step>E</step><octave>4</octave></pitch><duration>4</duration><voice>1</voice><notations><tied type="stop"/></notations></note>
<note><rest/><duration>8</duration><voice>1</voice></note>
</measure>
</part>
</score-partwise>"#;

    #[test]
    fn parallel_tied_chord_tones_merge_per_chain() {
        use super::super::bekern::to_bekern;
        use super::super::musicxml_import::{bekern_events, staff_headers};
        let headers = staff_headers(PARALLEL_TIES).unwrap();
        let events = bekern_events(PARALLEL_TIES).unwrap();
        let (toks, _) = to_bekern(&events, &headers).unwrap();
        let hyp = decode_bekern(&toks.join(" ")).unwrap();
        let mut got: Vec<(String, i64, i64)> = hyp.parts[0]
            .notes
            .iter()
            .filter(|n| !n.rest)
            .map(|n| {
                (
                    n.pitch.clone().unwrap(),
                    (n.onset * 480.0).round() as i64,
                    (n.dur * 480.0).round() as i64,
                )
            })
            .collect();
        got.sort();
        assert_eq!(
            got,
            vec![("C4".to_string(), 0, 960), ("E4".to_string(), 0, 960),]
        );
    }

    // ds0026 m51->m52: a triplet left open at the barline (its stop
    // printed in m51 on another staff) meets new notes carrying a
    // different ratio with no start marker. The old bracket ended; the
    // new notes belong to a new unmarked bracket.
    const STALE_REOPEN_FIXTURE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<score-partwise version="3.1">
<part-list><score-part id="P1"><part-name>Voice</part-name></score-part></part-list>
<part id="P1">
<measure number="1">
<attributes><divisions>12</divisions><clef><sign>G</sign><line>2</line></clef><key><fifths>0</fifths></key><time><beats>4</beats><beat-type>4</beat-type></time></attributes>
<note><pitch><step>C</step><octave>4</octave></pitch><duration>4</duration><voice>1</voice><type>eighth</type><time-modification><actual-notes>3</actual-notes><normal-notes>2</normal-notes></time-modification><notations><tuplet type="start"/></notations></note>
<note><pitch><step>D</step><octave>4</octave></pitch><duration>4</duration><voice>1</voice><type>eighth</type><time-modification><actual-notes>3</actual-notes><normal-notes>2</normal-notes></time-modification></note>
<note><pitch><step>E</step><octave>4</octave></pitch><duration>4</duration><voice>1</voice><type>eighth</type><time-modification><actual-notes>3</actual-notes><normal-notes>2</normal-notes></time-modification></note>
<note><pitch><step>F</step><octave>4</octave></pitch><duration>4</duration><voice>1</voice><type>eighth</type><time-modification><actual-notes>3</actual-notes><normal-notes>2</normal-notes></time-modification></note>
<note><pitch><step>G</step><octave>4</octave></pitch><duration>4</duration><voice>1</voice><type>eighth</type><time-modification><actual-notes>3</actual-notes><normal-notes>2</normal-notes></time-modification></note>
<note><pitch><step>A</step><octave>4</octave></pitch><duration>4</duration><voice>1</voice><type>eighth</type><time-modification><actual-notes>3</actual-notes><normal-notes>2</normal-notes></time-modification></note>
<note><pitch><step>B</step><octave>4</octave></pitch><duration>4</duration><voice>1</voice><type>eighth</type><time-modification><actual-notes>3</actual-notes><normal-notes>2</normal-notes></time-modification></note>
<note><pitch><step>C</step><octave>5</octave></pitch><duration>4</duration><voice>1</voice><type>eighth</type><time-modification><actual-notes>3</actual-notes><normal-notes>2</normal-notes></time-modification></note>
<note><pitch><step>D</step><octave>5</octave></pitch><duration>4</duration><voice>1</voice><type>eighth</type><time-modification><actual-notes>3</actual-notes><normal-notes>2</normal-notes></time-modification></note>
<note><pitch><step>E</step><octave>5</octave></pitch><duration>4</duration><voice>1</voice><type>eighth</type><time-modification><actual-notes>3</actual-notes><normal-notes>2</normal-notes></time-modification></note>
<note><pitch><step>F</step><octave>5</octave></pitch><duration>4</duration><voice>1</voice><type>eighth</type><time-modification><actual-notes>3</actual-notes><normal-notes>2</normal-notes></time-modification></note>
<note><pitch><step>G</step><octave>5</octave></pitch><duration>4</duration><voice>1</voice><type>eighth</type><time-modification><actual-notes>3</actual-notes><normal-notes>2</normal-notes></time-modification></note>
</measure>
<measure number="2">
<note><pitch><step>G</step><octave>4</octave></pitch><duration>12</duration><voice>1</voice><type>quarter</type><time-modification><actual-notes>2</actual-notes><normal-notes>1</normal-notes></time-modification></note>
<note><pitch><step>A</step><octave>4</octave></pitch><duration>12</duration><voice>1</voice><type>quarter</type></note>
<note><rest/><duration>24</duration><voice>1</voice><type>half</type></note>
</measure>
</part>
</score-partwise>"#;

    // Genuine nesting: an explicitly marked (2,1) run stays open
    // while a triplet of 16ths starts inside it after an event-less
    // silence (`<forward>`, so the cursor truly lags and the
    // sequential rule does not fire). Both levels must survive, and
    // the triplet 16ths must sound at a sixth of a beat (40 ticks).
    const NESTED_TUPLETS_FIXTURE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<score-partwise version="3.1">
<part-list><score-part id="P1"><part-name>Voice</part-name></score-part></part-list>
<part id="P1">
<measure number="1">
<attributes><divisions>12</divisions><clef><sign>G</sign><line>2</line></clef><key><fifths>0</fifths></key><time><beats>4</beats><beat-type>4</beat-type></time></attributes>
<note><pitch><step>A</step><octave>4</octave></pitch><duration>12</duration><voice>1</voice><type>half</type><time-modification><actual-notes>2</actual-notes><normal-notes>1</normal-notes></time-modification><notations><tuplet type="start"/></notations></note>
<forward><duration>12</duration><voice>1</voice></forward>
<note><pitch><step>B</step><octave>4</octave></pitch><duration>2</duration><voice>1</voice><type>16th</type><time-modification><actual-notes>3</actual-notes><normal-notes>2</normal-notes></time-modification><notations><tuplet type="start"/></notations></note>
<note><pitch><step>C</step><octave>5</octave></pitch><duration>2</duration><voice>1</voice><type>16th</type><time-modification><actual-notes>3</actual-notes><normal-notes>2</normal-notes></time-modification></note>
<note><pitch><step>D</step><octave>5</octave></pitch><duration>2</duration><voice>1</voice><type>16th</type><time-modification><actual-notes>3</actual-notes><normal-notes>2</normal-notes></time-modification><notations><tuplet type="stop"/></notations></note>
</measure>
</part>
</score-partwise>"#;

    fn pitched_ticks(score: &super::super::ScoreInput) -> Vec<(String, i64, i64)> {
        let mut got: Vec<(String, i64, i64)> = score.parts[0]
            .notes
            .iter()
            .filter(|n| !n.rest)
            .map(|n| {
                (
                    n.pitch.clone().unwrap(),
                    (n.onset * 480.0).round() as i64,
                    (n.dur * 480.0).round() as i64,
                )
            })
            .collect();
        got.sort();
        got
    }

    #[test]
    fn ratio_change_across_barline_drops_stale_reopen() {
        use super::super::bekern::to_bekern;
        use super::super::musicxml_import::{bekern_events, staff_headers};
        let headers = staff_headers(STALE_REOPEN_FIXTURE).unwrap();
        let events = bekern_events(STALE_REOPEN_FIXTURE).unwrap();
        let (toks, _) = to_bekern(&events, &headers).unwrap();
        // The m1 triplet opens once; it must not reopen in m2 under the
        // (2,1) duplets. The duplet opens its own implicit span instead.
        assert_eq!(
            toks.iter().filter(|t| *t == "tuplet-start-3-2").count(),
            1,
            "stale (3,2) reattached in m2: {toks:?}"
        );
        assert_eq!(toks.iter().filter(|t| *t == "tuplet-start-2-1").count(), 1);
        // The duplet quarter is written as a half (2.0 beats) and sounds
        // half of that (1.0 = 480 ticks); the plain quarter that follows
        // sounds whole (480).
        let hyp = decode_bekern(&toks.join(" ")).unwrap();
        assert_eq!(
            pitched_ticks(&hyp),
            vec![
                ("A4".to_string(), 800, 160),
                ("A4".to_string(), 2400, 480),
                ("B4".to_string(), 960, 160),
                ("C4".to_string(), 0, 160),
                ("C5".to_string(), 1120, 160),
                ("D4".to_string(), 160, 160),
                ("D5".to_string(), 1280, 160),
                ("E4".to_string(), 320, 160),
                ("E5".to_string(), 1440, 160),
                ("F4".to_string(), 480, 160),
                ("F5".to_string(), 1600, 160),
                ("G4".to_string(), 640, 160),
                ("G4".to_string(), 1920, 480),
                ("G5".to_string(), 1760, 160),
            ]
        );
    }

    #[test]
    fn explicit_start_after_gap_nests_under_open_span() {
        use super::super::bekern::to_bekern;
        use super::super::musicxml_import::{bekern_events, staff_headers};
        let headers = staff_headers(NESTED_TUPLETS_FIXTURE).unwrap();
        let events = bekern_events(NESTED_TUPLETS_FIXTURE).unwrap();
        let (toks, _) = to_bekern(&events, &headers).unwrap();
        // The inner triplet starts after a real silence gap, so the
        // sequential rule does not fire: both levels stay open and the
        // triplet 16ths sound at a twelfth of a beat (40 ticks).
        assert_eq!(
            toks.iter().filter(|t| *t == "tuplet-start-2-1").count(),
            1,
            "tokens: {toks:?}"
        );
        assert_eq!(toks.iter().filter(|t| *t == "tuplet-start-3-2").count(), 1);
        let hyp = decode_bekern(&toks.join(" ")).unwrap();
        assert_eq!(
            pitched_ticks(&hyp),
            vec![
                ("A4".to_string(), 0, 480),
                ("B4".to_string(), 960, 40),
                ("C5".to_string(), 1040, 40),
                ("D5".to_string(), 1120, 40),
            ],
            "tokens: {toks:?}"
        );
    }

    // ds0226 m35: an unmarked (2,1) run ends exactly at beat 2.0 and
    // an explicitly marked (3,2) triplet starts there (sequential
    // brackets, not nesting). Nesting the triplet under the still-open
    // duplet triple-scales below and dust-fails the gap fill.
    const SEQUENTIAL_TUPLETS_FIXTURE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<score-partwise version="3.1">
<part-list><score-part id="P1"><part-name>Voice</part-name></score-part></part-list>
<part id="P1">
<measure number="1">
<attributes><divisions>12</divisions><clef><sign>G</sign><line>2</line></clef><key><fifths>0</fifths></key><time><beats>4</beats><beat-type>4</beat-type></time></attributes>
<note><pitch><step>A</step><octave>4</octave></pitch><duration>3</duration><voice>1</voice><type>eighth</type><time-modification><actual-notes>2</actual-notes><normal-notes>1</normal-notes></time-modification></note>
<note><pitch><step>B</step><octave>4</octave></pitch><duration>3</duration><voice>1</voice><type>eighth</type><time-modification><actual-notes>2</actual-notes><normal-notes>1</normal-notes></time-modification></note>
<note><pitch><step>C</step><octave>5</octave></pitch><duration>3</duration><voice>1</voice><type>eighth</type><time-modification><actual-notes>2</actual-notes><normal-notes>1</normal-notes></time-modification></note>
<note><pitch><step>D</step><octave>5</octave></pitch><duration>3</duration><voice>1</voice><type>eighth</type><time-modification><actual-notes>2</actual-notes><normal-notes>1</normal-notes></time-modification></note>
<note><pitch><step>E</step><octave>5</octave></pitch><duration>3</duration><voice>1</voice><type>eighth</type><time-modification><actual-notes>2</actual-notes><normal-notes>1</normal-notes></time-modification></note>
<note><pitch><step>F</step><octave>5</octave></pitch><duration>3</duration><voice>1</voice><type>eighth</type><time-modification><actual-notes>2</actual-notes><normal-notes>1</normal-notes></time-modification></note>
<note><pitch><step>G</step><octave>5</octave></pitch><duration>3</duration><voice>1</voice><type>eighth</type><time-modification><actual-notes>2</actual-notes><normal-notes>1</normal-notes></time-modification></note>
<note><pitch><step>A</step><octave>5</octave></pitch><duration>3</duration><voice>1</voice><type>eighth</type><time-modification><actual-notes>2</actual-notes><normal-notes>1</normal-notes></time-modification></note>
<note><pitch><step>B</step><octave>5</octave></pitch><duration>4</duration><voice>1</voice><type>eighth</type><time-modification><actual-notes>3</actual-notes><normal-notes>2</normal-notes></time-modification><notations><tuplet type="start"/></notations></note>
<note><pitch><step>C</step><octave>6</octave></pitch><duration>4</duration><voice>1</voice><type>eighth</type><time-modification><actual-notes>3</actual-notes><normal-notes>2</normal-notes></time-modification></note>
<note><pitch><step>D</step><octave>6</octave></pitch><duration>4</duration><voice>1</voice><type>eighth</type><time-modification><actual-notes>3</actual-notes><normal-notes>2</normal-notes></time-modification><notations><tuplet type="stop"/></notations></note>
<note><rest/><duration>12</duration><voice>1</voice><type>quarter</type></note>
</measure>
</part>
</score-partwise>"#;

    #[test]
    fn explicit_start_after_abutting_run_closes_stale_span() {
        use super::super::bekern::to_bekern;
        use super::super::musicxml_import::{bekern_events, staff_headers};
        let headers = staff_headers(SEQUENTIAL_TUPLETS_FIXTURE).unwrap();
        let events = bekern_events(SEQUENTIAL_TUPLETS_FIXTURE).unwrap();
        // Pre-fix this errors (nested scale-3 dust: untypeable 0.498).
        let (toks, _) = to_bekern(&events, &headers).unwrap();
        // The duplet opens once and closes before the triplet; the
        // triplet opens once at true scale.
        assert_eq!(toks.iter().filter(|t| *t == "tuplet-start-2-1").count(), 1);
        assert_eq!(toks.iter().filter(|t| *t == "tuplet-start-3-2").count(), 1);
        // Duplet eighths sound as 16ths (120 ticks); triplet eighths
        // sound at a true third (160 ticks).
        let hyp = decode_bekern(&toks.join(" ")).unwrap();
        assert_eq!(
            pitched_ticks(&hyp),
            vec![
                ("A4".to_string(), 0, 120),
                ("A5".to_string(), 840, 120),
                ("B4".to_string(), 120, 120),
                ("B5".to_string(), 960, 160),
                ("C5".to_string(), 240, 120),
                ("C6".to_string(), 1120, 160),
                ("D5".to_string(), 360, 120),
                ("D6".to_string(), 1280, 160),
                ("E5".to_string(), 480, 120),
                ("F5".to_string(), 600, 120),
                ("G5".to_string(), 720, 120),
            ]
        );
    }

    // Accepted S2 residual (arch-review-gap-encoding §5): an
    // explicitly starting triplet lands exactly on the unmarked duplet
    // gridline (abutting) and duplet members resume after it closes.
    // If the duplet genuinely contains the triplet (aligned nesting),
    // S2 flattens it today: the duplet closes, the triplet decodes at
    // top level, the resumed duplet re-opens implicitly. The §2 source
    // query (explicit start strictly inside another bracket's interval
    // on the same key) is the instrument that keeps this bounded; if
    // it ever finds genuine aligned nesting in the wild, this test
    // names the culprit mechanism and becomes the lookahead fix's
    // acceptance criterion.
    const S2_RESIDUAL_FIXTURE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<score-partwise version="3.1">
<part-list><score-part id="P1"><part-name>Voice</part-name></score-part></part-list>
<part id="P1">
<measure number="1">
<attributes><divisions>12</divisions><clef><sign>G</sign><line>2</line></clef><key><fifths>0</fifths></key><time><beats>4</beats><beat-type>4</beat-type></time></attributes>
<note><pitch><step>A</step><octave>4</octave></pitch><duration>12</duration><voice>1</voice><type>quarter</type><time-modification><actual-notes>2</actual-notes><normal-notes>1</normal-notes></time-modification></note>
<note><pitch><step>B</step><octave>4</octave></pitch><duration>4</duration><voice>1</voice><type>eighth</type><time-modification><actual-notes>3</actual-notes><normal-notes>2</normal-notes></time-modification><notations><tuplet type="start"/></notations></note>
<note><pitch><step>C</step><octave>5</octave></pitch><duration>4</duration><voice>1</voice><type>eighth</type><time-modification><actual-notes>3</actual-notes><normal-notes>2</normal-notes></time-modification></note>
<note><pitch><step>D</step><octave>5</octave></pitch><duration>4</duration><voice>1</voice><type>eighth</type><time-modification><actual-notes>3</actual-notes><normal-notes>2</normal-notes></time-modification><notations><tuplet type="stop"/></notations></note>
<note><pitch><step>G</step><octave>4</octave></pitch><duration>12</duration><voice>1</voice><type>quarter</type><time-modification><actual-notes>2</actual-notes><normal-notes>1</normal-notes></time-modification></note>
<note><rest/><duration>12</duration><voice>1</voice><type>quarter</type></note>
</measure>
</part>
</score-partwise>"#;

    #[test]
    fn abutting_explicit_start_flattens_outer_span() {
        use super::super::bekern::to_bekern;
        use super::super::musicxml_import::{bekern_events, staff_headers};
        let headers = staff_headers(S2_RESIDUAL_FIXTURE).unwrap();
        let events = bekern_events(S2_RESIDUAL_FIXTURE).unwrap();
        let (toks, _) = to_bekern(&events, &headers).unwrap();
        // The duplet opens, closes at the abutting triplet start, and
        // re-opens implicitly for the resumed member: two 2-1 spans,
        // one top-level 3-2 triplet.
        assert_eq!(
            toks.iter().filter(|t| *t == "tuplet-start-2-1").count(),
            2,
            "tokens: {toks:?}"
        );
        assert_eq!(
            toks.iter().filter(|t| *t == "tuplet-start-3-2").count(),
            1,
            "tokens: {toks:?}"
        );
        let hyp = decode_bekern(&toks.join(" ")).unwrap();
        assert_eq!(
            pitched_ticks(&hyp),
            vec![
                ("A4".to_string(), 0, 480),
                ("B4".to_string(), 480, 160),
                ("C5".to_string(), 640, 160),
                ("D5".to_string(), 800, 160),
                ("G4".to_string(), 960, 480),
            ],
            "tokens: {toks:?}"
        );
    }

    #[test]
    fn unison_ties_across_voices_merge_independently() {
        // ds0462 m19: two voices tie the same pitch (unison C5) with
        // independent chains. Pitch-only keying lets the second
        // tie-start overwrite the first, emitting a [0..600] fragment
        // plus a literal [720..1200]; both chains must merge instead.
        let score = decode_bekern(
            "clef-G-2 key+0 time-4-4 voice-1 quarter C 5 dot tie-start \
             voice-2 quarter E 4 chord quarter C 5 tie-start \
             voice-2 16th C 5 tie-stop voice-1 quarter C 5 tie-stop bar",
        )
        .unwrap();
        let mut got: Vec<(String, i64, i64)> = score.parts[0]
            .notes
            .iter()
            .filter(|n| !n.rest)
            .map(|n| {
                (
                    n.pitch.clone().unwrap(),
                    (n.onset * 480.0).round() as i64,
                    (n.dur * 480.0).round() as i64,
                )
            })
            .collect();
        got.sort();
        assert_eq!(
            got,
            vec![
                ("C5".to_string(), 0, 600),
                ("C5".to_string(), 0, 1200),
                ("E4".to_string(), 0, 480),
            ]
        );
    }

    #[test]
    fn cross_bar_tie_merges_when_seg_churns() {
        // Same voice tied across a barline whose segment number changes
        // (voice entry order churn): with exactly one open chain the
        // stop falls back to it. Pin: pitch-only keying already merged
        // this; the stack must not regress it.
        let score = decode_bekern(
            "clef-G-2 key+0 time-4-4 voice-1 quarter C 5 tie-start bar \
             voice-2 half C 5 tie-stop bar",
        )
        .unwrap();
        let mut got: Vec<(String, i64, i64)> = score.parts[0]
            .notes
            .iter()
            .filter(|n| !n.rest)
            .map(|n| {
                (
                    n.pitch.clone().unwrap(),
                    (n.onset * 480.0).round() as i64,
                    (n.dur * 480.0).round() as i64,
                )
            })
            .collect();
        got.sort();
        assert_eq!(got, vec![("C5".to_string(), 0, 1440)]);
    }

    #[test]
    fn ambiguous_overlap_resolves_lifo_deterministically() {
        // Two open same-pitch chains, stop on a third segment: the
        // stack top wins, and repeated decodes agree. The surviving
        // chain flushes literally at end of stream.
        let toks = "clef-G-2 key+0 time-4-4 voice-1 quarter C 5 tie-start \
             voice-2 half C 5 tie-start voice-3 whole C 5 tie-stop bar";
        let oto = |s: &str| {
            let mut got: Vec<(String, i64, i64)> = decode_bekern(s).unwrap().parts[0]
                .notes
                .iter()
                .filter(|n| !n.rest)
                .map(|n| {
                    (
                        n.pitch.clone().unwrap(),
                        (n.onset * 480.0).round() as i64,
                        (n.dur * 480.0).round() as i64,
                    )
                })
                .collect();
            got.sort();
            got
        };
        let want = vec![("C5".to_string(), 0, 480), ("C5".to_string(), 0, 2880)];
        assert_eq!(oto(toks), want);
        assert_eq!(oto(toks), want);
    }

    #[test]
    fn triplet_decodes_to_performed_onsets() {
        // Three triplet eighths in the time of one quarter: performed
        // onsets 0, 1/3, 2/3 (not the written 0, 1/2, 1).
        let score = decode_bekern(
            "clef-G-2 key+0 time-4-4 voice-1 \
             tuplet-start-3-2 eighth C 4 eighth D 4 a-1 eighth E 4 tuplet-stop \
             quarter F 4 bar",
        )
        .unwrap();
        let notes = &score.parts[0].notes;
        assert_eq!(notes.len(), 4);
        let onsets: Vec<f64> = notes.iter().map(|n| n.onset).collect();
        assert!(approx(onsets[0], 0.0), "{onsets:?}");
        assert!(approx(onsets[1], 1.0 / 3.0), "{onsets:?}");
        assert!(approx(onsets[2], 2.0 / 3.0), "{onsets:?}");
        assert!(approx(onsets[3], 1.0), "{onsets:?}");
        assert!(approx(notes[1].dur, 1.0 / 3.0));
        assert_eq!(notes[0].pitch.as_deref(), Some("C4"));
        // Alter token sits before the octave in pitch spelling.
        assert_eq!(notes[1].pitch.as_deref(), Some("Db4"));
    }

    #[test]
    fn ties_merge_and_bar_rest_jumps_to_bar_end() {
        let score = decode_bekern(
            "clef-G-2 key+0 time-4-4 voice-1 \
             quarter C 4 tie-start quarter C 4 tie-stop \
             bar-rest voice-2 quarter D 4 bar",
        )
        .unwrap();
        let notes = &score.parts[0].notes;
        // Tie chain merges (2.0 beats). The bar rest fills voice 1; the
        // marker opens voice 2, so D starts at offset 0. Rests are kept
        // as rest notes.
        assert_eq!(notes.len(), 3);
        assert!(approx(notes[0].onset, 0.0));
        assert!(approx(notes[0].dur, 2.0));
        assert_eq!(notes[0].voice, 1);
        assert!(notes[1].rest);
        assert!(approx(notes[2].onset, 0.0));
        assert_eq!(notes[2].voice, 2);
        assert_eq!(notes[2].pitch.as_deref(), Some("D4"));
    }

    #[test]
    fn unmerged_decode_preserves_tie_links() {
        // Same stream, both semantics: merged collapses the chain
        // (note-list path), unmerged keeps every link with its flags
        // (notation path for the strict-xml oracle).
        use super::decode_bekern_unmerged;
        let toks = "clef-G-2 key+0 time-4-4 voice-1 \
                    quarter C 4 tie-start bar quarter C 4 tie-stop bar";
        let merged = decode_bekern(toks).unwrap();
        assert_eq!(merged.parts[0].notes.len(), 1);
        assert!(approx(merged.parts[0].notes[0].dur, 2.0));
        let unmerged = decode_bekern_unmerged(toks).unwrap();
        let notes = &unmerged.parts[0].notes;
        assert_eq!(notes.len(), 2);
        assert!(approx(notes[0].onset, 0.0));
        assert!(approx(notes[0].dur, 1.0));
        assert!(notes[0].tie_start && !notes[0].tie_stop);
        assert!(approx(notes[1].onset, 4.0));
        assert!(approx(notes[1].dur, 1.0));
        assert!(notes[1].tie_stop && !notes[1].tie_start);
    }

    #[test]
    fn unmerged_decode_keeps_grace_zero_width() {
        // A grace eighth steals no time: the following quarter keeps
        // the grace's onset, and the grace carries its display
        // duration. The merged path drops grace (its reference has
        // none) but still records the onset for chord runs.
        use super::decode_bekern_unmerged;
        let toks = "clef-G-2 key+0 time-4-4 voice-1 \
                    grace eighth E 5 quarter C 5 chord quarter G 4 bar";
        let unmerged = decode_bekern_unmerged(toks).unwrap();
        let notes = &unmerged.parts[0].notes;
        assert_eq!(notes.len(), 3);
        assert!(notes[0].grace);
        assert_eq!(notes[0].pitch.as_deref(), Some("E5"));
        assert!(approx(notes[0].dur, 0.5));
        assert!(approx(notes[1].onset, notes[0].onset));
        assert!(!notes[1].grace);
        // The chord tone inherits the root's non-grace run.
        assert!(!notes[2].grace);
        assert!(approx(notes[2].onset, notes[1].onset));
        let merged = decode_bekern(toks).unwrap();
        // Grace dropped; quarter + chord tone remain.
        assert_eq!(merged.parts[0].notes.len(), 2);
        assert!(!merged.parts[0].notes[0].grace);
    }

    /// Synthetic bekern+ score: non-dyadic divisions with a jittered
    /// triplet member (tick quantum), a grace note, slurs, a staccato,
    /// a rest fermata, dynamics, hairpins, a double bar, a final bar,
    /// and a mid-score key/time/clef change. Exercises the full
    /// MusicXML -> bekern+ -> decode -> MusicXML path.
    const PLUS_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<score-partwise version="3.1">
  <part-list><score-part id="P1"><part-name>Solo</part-name></score-part></part-list>
  <part id="P1">
    <measure number="1">
      <attributes><divisions>10080</divisions><key><fifths>0</fifths></key><time><beats>4</beats><beat-type>4</beat-type></time><clef><sign>G</sign><line>2</line></clef></attributes>
      <note><pitch><step>C</step><octave>4</octave></pitch><duration>10080</duration><voice>1</voice><type>quarter</type></note>
      <note><grace/><pitch><step>D</step><octave>5</octave></pitch><voice>1</voice><type>eighth</type></note>
      <note><pitch><step>E</step><octave>4</octave></pitch><duration>10080</duration><voice>1</voice><type>quarter</type><notations><slur type="start" number="1"/><articulations><staccato/></articulations></notations></note>
      <note><pitch><step>F</step><octave>4</octave></pitch><duration>10080</duration><voice>1</voice><type>quarter</type><tie type="start"/><notations><slur type="stop" number="1"/></notations></note>
      <direction placement="below"><direction-type><dynamics><f/></dynamics></direction-type></direction>
      <direction placement="below"><direction-type><wedge type="crescendo"/></direction-type></direction>
    </measure>
    <measure number="2">
      <attributes><key><fifths>2</fifths></key><time><beats>3</beats><beat-type>4</beat-type></time><clef><sign>F</sign><line>4</line></clef></attributes>
      <note><pitch><step>F</step><octave>4</octave></pitch><duration>6720</duration><voice>1</voice><type>quarter</type><time-modification><actual-notes>3</actual-notes><normal-notes>2</normal-notes></time-modification><tie type="stop"/><notations><tuplet type="start" number="1" bracket="yes"/></notations></note>
      <note><pitch><step>F</step><octave>4</octave></pitch><duration>6710</duration><voice>1</voice><type>quarter</type><time-modification><actual-notes>3</actual-notes><normal-notes>2</normal-notes></time-modification></note>
      <note><pitch><step>F</step><octave>4</octave></pitch><duration>6730</duration><voice>1</voice><type>quarter</type><time-modification><actual-notes>3</actual-notes><normal-notes>2</normal-notes></time-modification><notations><tuplet type="stop" number="1"/></notations></note>
      <barline location="right"><bar-style>light-light</bar-style></barline>
    </measure>
    <measure number="3">
      <note><pitch><step>A</step><octave>3</octave></pitch><duration>20160</duration><voice>1</voice><type>half</type></note>
      <note><rest/><duration>10080</duration><voice>1</voice><type>quarter</type><notations><fermata/></notations></note>
      <direction placement="below"><direction-type><wedge type="stop"/></direction-type></direction>
      <barline location="right"><bar-style>light-heavy</bar-style></barline>
    </measure>
  </part>
</score-partwise>"#;

    /// Scorer-style comparison: exact measure + MIDI, tolerant onset +
    /// duration (tick-quantum jitter never exceeds a few 10^-3 beats).
    fn events_keyed(xml: &str) -> Vec<(u32, f64, i32, f64)> {
        use super::super::musicxml_import::score_events;
        score_events(xml)
            .unwrap()
            .into_iter()
            .map(|e| (e.measure, e.onset, e.midi, e.dur))
            .collect()
    }

    fn assert_events_close(got: &[(u32, f64, i32, f64)], want: &[(u32, f64, i32, f64)]) {
        assert_eq!(got.len(), want.len(), "event count: {got:?} vs {want:?}");
        for (g, w) in got.iter().zip(want.iter()) {
            assert_eq!(g.0, w.0, "measure for {g:?} vs {w:?}");
            assert!((g.1 - w.1).abs() < 0.01, "onset for {g:?} vs {w:?}");
            assert_eq!(g.2, w.2, "midi for {g:?} vs {w:?}");
            assert!((g.3 - w.3).abs() < 0.01, "dur for {g:?} vs {w:?}");
        }
    }

    #[test]
    fn bekern_plus_marks_round_trip() {
        // MusicXML -> bekern+ -> unmerged decode -> MusicXML preserves
        // every pitched event (grace included) and re-renders every
        // mark the fixture carries.
        use super::super::bekern::to_bekern;
        use super::super::musicxml_import::{bekern_events, staff_headers};
        use super::super::{parse_score, to_musicxml, LossTable};
        use super::decode_bekern_unmerged;
        let headers = staff_headers(PLUS_XML).unwrap();
        let events = bekern_events(PLUS_XML).unwrap();
        let (toks, graces) = to_bekern(&events, &headers).unwrap();
        assert_eq!(graces, 0, "only grace rests skip; the fixture has none");
        let joined = toks.join(" ");
        for mark in [
            "grace eighth D 5",
            "slur-start",
            "slur-stop",
            "stacc",
            "fermata",
            "dyn-f",
            "hairpin-cresc-start",
            "hairpin-stop",
            "bar-double",
            "bar-light-heavy",
            "clef-F-4",
            "key+2",
            "time-3-4",
            "tie-start",
            "tie-stop",
            "tuplet-start-3-2",
            "tuplet-stop",
        ] {
            assert!(
                joined.contains(mark),
                "token stream carries {mark}: {joined}"
            );
        }
        let input = decode_bekern_unmerged(&joined).unwrap();
        // Mid-stream state lands on the right measures.
        let at = |m: u32| input.measure_attrs.iter().find(|a| a.measure == m);
        assert_eq!(at(2).and_then(|a| a.key_fifths), Some(2));
        assert_eq!(
            at(2).and_then(|a| a.time),
            Some(super::super::TimeSig {
                beats: 3,
                beat_type: 4
            })
        );
        assert_eq!(at(2).and_then(|a| a.clef_sign), Some('F'));
        assert_eq!(at(2).and_then(|a| a.clef_line), Some(4));
        assert_eq!(at(1).and_then(|a| a.dynamic.as_deref()), Some("f"));
        assert_eq!(at(1).and_then(|a| a.hairpin.as_deref()), Some("cresc"));
        assert_eq!(
            at(2).and_then(|a| a.barline_right.as_deref()),
            Some("double")
        );
        assert_eq!(at(3).and_then(|a| a.hairpin.as_deref()), Some("stop"));
        assert_eq!(
            at(3).and_then(|a| a.barline_right.as_deref()),
            Some("light-heavy")
        );
        let (score, losses) = parse_score(&input).unwrap();
        assert!(losses.entries.is_empty(), "unexpected losses: {losses:?}");
        let hyp = to_musicxml(&score, &mut LossTable::default());
        for frag in [
            "<grace/>",
            "<slur type=\"start\"",
            "<slur type=\"stop\"",
            "<staccato/>",
            "<fermata/>",
            "<dynamics><f/></dynamics>",
            "<wedge type=\"crescendo\"/>",
            "<wedge type=\"stop\"/>",
            "<bar-style>light-light</bar-style>",
            "<bar-style>light-heavy</bar-style>",
            "<time><beats>3</beats><beat-type>4</beat-type></time>",
            "<key><fifths>2</fifths></key>",
            "<clef><sign>F</sign><line>4</line></clef>",
            "<tie type=\"start\"/>",
            "<tie type=\"stop\"/>",
        ] {
            assert!(hyp.contains(frag), "rendered MusicXML carries {frag}");
        }
        assert_events_close(&events_keyed(&hyp), &events_keyed(PLUS_XML));
    }

    #[test]
    fn pickup_measure_has_no_trailing_padding() {
        // An anacrusis must not force trailing padding rests: the token
        // stream carries no `bar-rest` for the short measure, and the
        // next measure still lands on the grid.
        use super::super::bekern::to_bekern;
        use super::super::musicxml_import::{bekern_events, staff_headers};
        use super::super::{parse_score, to_musicxml, LossTable};
        use super::decode_bekern_unmerged;
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<score-partwise version="3.1">
  <part-list><score-part id="P1"><part-name>Solo</part-name></score-part></part-list>
  <part id="P1">
    <measure number="1">
      <attributes><divisions>480</divisions><key><fifths>0</fifths></key><time><beats>4</beats><beat-type>4</beat-type></time><clef><sign>G</sign><line>2</line></clef></attributes>
      <note><pitch><step>C</step><octave>4</octave></pitch><duration>480</duration><voice>1</voice><type>quarter</type></note>
    </measure>
    <measure number="2">
      <note><pitch><step>D</step><octave>4</octave></pitch><duration>960</duration><voice>1</voice><type>half</type></note>
      <note><pitch><step>E</step><octave>4</octave></pitch><duration>960</duration><voice>1</voice><type>half</type></note>
    </measure>
  </part>
</score-partwise>"#;
        let headers = staff_headers(xml).unwrap();
        let events = bekern_events(xml).unwrap();
        let (toks, _) = to_bekern(&events, &headers).unwrap();
        assert!(
            !toks.iter().any(|t| t == "bar-rest"),
            "no padding rests forced: {toks:?}"
        );
        let input = decode_bekern_unmerged(&toks.join(" ")).unwrap();
        let d4 = input.parts[0]
            .notes
            .iter()
            .find(|n| n.pitch.as_deref() == Some("D4"))
            .expect("D4 decodes");
        assert!(approx(d4.onset, 4.0), "measure 2 lands on the grid");
        let (score, _) = parse_score(&input).unwrap();
        let hyp = to_musicxml(&score, &mut LossTable::default());
        assert_events_close(&events_keyed(&hyp), &events_keyed(xml));
    }

    #[test]
    fn pickup_and_midstream_1_32_time_change_preserves_clock() {
        // Anacrusis pickup in 4/4 followed by a 1/32 bar and return to 4/4
        // (fiddle-strathspey pattern): decode_bekern must advance bar_start
        // by each measure's active time signature (4.0, 0.125, 4.0) so it
        // matches score_input_from_musicxml, and to_musicxml + validate_musicxml
        // must preserve the 1/32 measure without bogus rest padding.
        use super::super::bekern::to_bekern;
        use super::super::musicxml_import::{
            bekern_events, score_input_from_musicxml, staff_headers,
        };
        use super::super::{parse_score, to_musicxml, validate_musicxml, LossTable};
        use super::decode_bekern_unmerged;
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<score-partwise version="3.1">
  <part-list><score-part id="P1"><part-name>Fiddle</part-name></score-part></part-list>
  <part id="P1">
    <measure number="0">
      <attributes><divisions>480</divisions><key><fifths>0</fifths></key><time><beats>4</beats><beat-type>4</beat-type></time><clef><sign>G</sign><line>2</line></clef></attributes>
      <note><pitch><step>E</step><octave>5</octave></pitch><duration>240</duration><voice>1</voice><type>eighth</type></note>
    </measure>
    <measure number="1">
      <attributes><time><beats>1</beats><beat-type>32</beat-type></time></attributes>
      <note><pitch><step>B</step><octave>4</octave></pitch><duration>60</duration><voice>1</voice><type>32nd</type></note>
    </measure>
    <measure number="2">
      <attributes><time><beats>4</beats><beat-type>4</beat-type></time></attributes>
      <note><pitch><step>A</step><octave>4</octave></pitch><duration>480</duration><voice>1</voice><type>quarter</type></note>
    </measure>
  </part>
</score-partwise>"#;
        let headers = staff_headers(xml).unwrap();
        let events = bekern_events(xml).unwrap();
        let (toks, _) = to_bekern(&events, &headers).unwrap();
        let dec = decode_bekern(&toks.join(" ")).unwrap();
        let imp = score_input_from_musicxml(xml).unwrap();
        let dec_onsets: Vec<(String, f64)> = dec.parts[0]
            .notes
            .iter()
            .filter(|n| !n.rest)
            .map(|n| (n.pitch.clone().unwrap(), n.onset))
            .collect();
        let imp_onsets: Vec<(String, f64)> = imp.parts[0]
            .notes
            .iter()
            .filter(|n| !n.rest)
            .map(|n| (n.pitch.clone().unwrap(), n.onset))
            .collect();
        assert_eq!(dec_onsets.len(), 3);
        assert!(approx(dec_onsets[0].1, 0.0));
        assert!(approx(dec_onsets[1].1, 4.0));
        assert!(approx(dec_onsets[2].1, 4.125));
        assert_eq!(
            dec_onsets, imp_onsets,
            "decode_bekern matches import onsets"
        );

        let unmerged = decode_bekern_unmerged(&toks.join(" ")).unwrap();
        let (score, _) = parse_score(&unmerged).unwrap();
        assert_eq!(
            score.measure_windows(),
            vec![(0, 1920), (1920, 1980), (1980, 3900)]
        );
        let hyp = to_musicxml(&score, &mut LossTable::default());
        let issues = validate_musicxml(&hyp);
        assert!(
            issues.iter().all(|i| i.severity != "error"),
            "validates cleanly: {issues:?}"
        );
        assert_events_close(&events_keyed(&hyp), &events_keyed(xml));
    }

    #[test]
    fn midstream_4_16_and_1_16_time_change_preserves_clock() {
        // Mid-stream 2/4 -> 4/16 -> 1/16 -> 2/4 time-signature transitions
        // (oneills-tune pattern): cumulative bar_start must advance by
        // 2.0, 1.0, 0.25, 2.0 beats.
        use super::super::{parse_score, to_musicxml, validate_musicxml, LossTable};
        use super::decode_bekern_unmerged;
        let stream = concat!(
            "clef-G-2 key-2 time-2-4 ",
            "voice-1 quarter G 4 quarter A 4 bar ",
            "time-4-16 voice-1 16th B 4 16th C 5 16th D 5 16th E 5 bar ",
            "time-1-16 voice-1 16th F 5 bar ",
            "time-2-4 voice-1 half G 5 bar"
        );
        let dec = decode_bekern(stream).unwrap();
        let pitched: Vec<(String, f64)> = dec.parts[0]
            .notes
            .iter()
            .filter(|n| !n.rest)
            .map(|n| (n.pitch.clone().unwrap(), n.onset))
            .collect();
        assert!(approx(pitched[0].1, 0.0)); // G4 in 2/4
        assert!(approx(pitched[2].1, 2.0)); // B4 in 4/16
        assert!(approx(pitched[6].1, 3.0)); // F5 in 1/16
        assert!(approx(pitched[7].1, 3.25)); // G5 back in 2/4

        let unmerged = decode_bekern_unmerged(stream).unwrap();
        let (score, _) = parse_score(&unmerged).unwrap();
        assert_eq!(
            score.measure_windows(),
            vec![(0, 960), (960, 1440), (1440, 1560), (1560, 2520)]
        );
        let xml = to_musicxml(&score, &mut LossTable::default());
        let issues = validate_musicxml(&xml);
        assert!(
            issues.iter().all(|i| i.severity != "error"),
            "validates cleanly: {issues:?}"
        );
    }

    #[test]
    fn multiple_dynamics_and_hairpins_in_same_measure_do_not_overwrite() {
        use super::super::{has_errors, pack_mscz, parse_score, to_mscx, to_musicxml, validate_mscz, validate_musicxml};
        use super::decode_bekern_unmerged;

        let stream = concat!(
            "clef-G-2 key+0 time-4-4 ",
            "dyn-p hairpin-cresc-start voice-1 quarter C 4 quarter D 4 ",
            "dyn-f hairpin-stop quarter E 4 quarter F 4 bar ",
            "dyn-mf hairpin-dim-start voice-1 half G 4 half A 4 bar ",
            "hairpin-stop dyn-pp voice-1 whole C 4 bar-light-heavy"
        );
        let dec = decode_bekern_unmerged(stream).unwrap();
        let m1 = dec.measure_attrs.iter().find(|a| a.measure == 1).unwrap();
        assert_eq!(m1.dynamic.as_deref(), Some("p"));
        assert_eq!(m1.all_dynamics(), vec!["p", "f"]);
        assert_eq!(m1.hairpin.as_deref(), Some("cresc"));
        assert_eq!(m1.all_hairpins(), vec!["cresc", "stop"]);

        let (score, mut losses) = parse_score(&dec).unwrap();
        let xml = to_musicxml(&score, &mut losses);
        assert!(!has_errors(&validate_musicxml(&xml)));
        assert!(xml.contains("<dynamics><p/></dynamics>"));
        assert!(xml.contains("<dynamics><f/></dynamics>"));
        assert!(xml.contains("<wedge type=\"crescendo\"/>"));
        assert!(xml.contains("<wedge type=\"stop\"/>"));

        let mscx = to_mscx(&score, &mut losses);
        assert!(mscx.contains("<subtype>p</subtype>"));
        assert!(mscx.contains("<subtype>f</subtype>"));
        // Both same-measure cresc (m1) and cross-measure dim (m2->m3) hairpins emit cleanly.
        assert!(mscx.matches("<Spanner type=\"HairPin\">").count() >= 4);
        let mscz_bytes = pack_mscz(&score, &mscx).unwrap();
        assert!(!has_errors(&validate_mscz(&mscz_bytes)));
    }

    #[test]
    fn multistaff_midpiece_clefs_emit_on_exact_staff_without_clobbering() {
        use super::super::{has_errors, parse_score, to_musicxml, validate_musicxml, LossTable};
        use super::decode_bekern_unmerged;

        // 1) 2-staff piano grand staff: LH (staff 2) switches to G-2 in measure 2
        //    (emitted in to_bekern bound order right before `voice-2`), while RH stays G-2.
        let piano = concat!(
            "clef-G-2 key+0 time-4-4 clef-F-4 key+0 time-4-4 ",
            "voice-1 whole C 5 voice-2 whole C 3 bar ",
            "voice-1 whole D 5 clef-G-2 voice-2 whole G 4 bar-light-heavy"
        );
        let dec = decode_bekern_unmerged(piano).unwrap();
        assert!(
            dec.parts[0].measure_attrs.is_empty(),
            "RH (staff 1) must not receive LH's mid-piece clef"
        );
        let lh_m2 = dec.parts[1]
            .measure_attrs
            .iter()
            .find(|a| a.measure == 2)
            .expect("LH (staff 2) must record mid-piece clef at measure 2");
        assert_eq!((lh_m2.clef_sign, lh_m2.clef_line), (Some('G'), Some(2)));

        let (score, mut losses) = parse_score(&dec).unwrap();
        let xml = to_musicxml(&score, &mut losses);
        assert!(!has_errors(&validate_musicxml(&xml)));
        // Measure 2 of the grand-staff part must emit <clef number="2"><sign>G</sign><line>2</line></clef>
        // and must NOT emit a redundant <clef number="1"> in measure 2.
        let m2_start = xml.find("<measure number=\"2\">").expect("measure 2");
        let m2_body = &xml[m2_start..];
        assert!(
            m2_body.contains("<clef number=\"2\"><sign>G</sign><line>2</line></clef>"),
            "measure 2 must emit staff-2 clef change:\n{m2_body}"
        );
        assert!(
            !m2_body.contains("<clef number=\"1\">"),
            "measure 2 must not clobber staff 1 clef:\n{m2_body}"
        );

        // 2) 4-staff string quartet: Cello (staff 4) switches to tenor clef (C-4) in measure 2
        //    (placed after `voice-4`), while Vln1/Vln2/Vla keep their clefs.
        let quartet = concat!(
            "clef-G-2 key+0 time-4-4 clef-G-2 key+0 time-4-4 ",
            "clef-C-3 key+0 time-4-4 clef-F-4 key+0 time-4-4 ",
            "voice-1 whole G 5 voice-2 whole D 5 voice-3 whole G 4 voice-4 whole G 2 bar ",
            "voice-1 whole A 5 voice-2 whole E 5 voice-3 whole A 4 voice-4 clef-C-4 whole D 4 bar-light-heavy"
        );
        let qdec = decode_bekern_unmerged(quartet).unwrap();
        assert!(qdec.parts[0].measure_attrs.is_empty());
        assert!(qdec.parts[1].measure_attrs.is_empty());
        assert!(qdec.parts[2].measure_attrs.is_empty());
        let vc_m2 = qdec.parts[3]
            .measure_attrs
            .iter()
            .find(|a| a.measure == 2)
            .expect("Cello (staff 4) must record mid-piece C-4 clef at measure 2");
        assert_eq!((vc_m2.clef_sign, vc_m2.clef_line), (Some('C'), Some(4)));

        let (qscore, _) = parse_score(&qdec).unwrap();
        let qxml = to_musicxml(&qscore, &mut LossTable::default());
        assert!(!has_errors(&validate_musicxml(&qxml)));
        assert_eq!(
            qxml.matches("<clef><sign>C</sign><line>4</line></clef>").count(),
            1,
            "mid-piece tenor clef must emit exactly once on Cello (P4), not broadcast to all parts"
        );
    }

    #[test]
    fn multistaff_dynamics_and_hairpins_attach_only_to_owning_part() {
        use super::super::{has_errors, parse_score, to_musicxml, validate_musicxml, LossTable};
        use super::decode_bekern_unmerged;

        // 4-staff string quartet: `dyn-f` on Violin 1 (before voice-1) and `hairpin-cresc-start`
        // + `dyn-p` on Cello (before voice-4). Neither may broadcast 4x across all parts!
        let quartet = concat!(
            "clef-G-2 key+0 time-4-4 clef-G-2 key+0 time-4-4 ",
            "clef-C-3 key+0 time-4-4 clef-F-4 key+0 time-4-4 ",
            "dyn-f voice-1 whole E 5 voice-2 whole C 5 voice-3 whole G 4 ",
            "dyn-p hairpin-cresc-start voice-4 whole C 3 bar-light-heavy"
        );
        let dec = decode_bekern_unmerged(quartet).unwrap();
        assert!(
            dec.measure_attrs.iter().all(|a| a.all_dynamics().is_empty() && a.all_hairpins().is_empty()),
            "multi-staff dynamics/hairpins must live on owning parts, not score-wide measure_attrs"
        );
        assert_eq!(dec.parts[0].measure_attrs[0].all_dynamics(), vec!["f"]);
        assert!(dec.parts[1].measure_attrs.is_empty());
        assert!(dec.parts[2].measure_attrs.is_empty());
        assert_eq!(dec.parts[3].measure_attrs[0].all_dynamics(), vec!["p"]);
        assert_eq!(dec.parts[3].measure_attrs[0].all_hairpins(), vec!["cresc"]);

        let (score, _) = parse_score(&dec).unwrap();
        let xml = to_musicxml(&score, &mut LossTable::default());
        assert!(!has_errors(&validate_musicxml(&xml)));
        assert_eq!(
            xml.matches("<dynamics><f/></dynamics>").count(),
            1,
            "dyn-f must emit exactly 1x (not 4x broadcast)"
        );
        assert_eq!(
            xml.matches("<dynamics><p/></dynamics>").count(),
            1,
            "dyn-p must emit exactly 1x (not 4x broadcast)"
        );
        assert_eq!(
            xml.matches("<wedge type=\"crescendo\"/>").count(),
            1,
            "hairpin-cresc-start must emit exactly 1x (not 4x broadcast)"
        );
    }

    #[test]
    fn zero_note_all_bar_rest_page_abstains_to_empty_score() {
        use super::super::{is_empty_score_input, to_empty_musicxml};
        use super::decode_bekern_unmerged;

        // Single-staff 8-bar all-bar-rest negative control (e.g. blank staff paper)
        let single_blank = concat!(
            "clef-G-2 key+0 time-4-4 ",
            "voice-1 bar-rest bar voice-1 bar-rest bar voice-1 bar-rest bar voice-1 bar-rest bar ",
            "voice-1 bar-rest bar voice-1 bar-rest bar voice-1 bar-rest bar voice-1 bar-rest bar"
        );
        let dec1 = decode_bekern_unmerged(single_blank).unwrap();
        assert!(
            is_empty_score_input(&dec1),
            "all-bar-rest single-staff page must abstain to empty ScoreInput"
        );

        // Multi-staff 6-bar all-bar-rest negative control
        let multi_blank = concat!(
            "clef-G-2 key+0 time-4-4 clef-F-4 key+0 time-4-4 ",
            "voice-1 bar-rest voice-2 bar-rest bar ",
            "voice-1 bar-rest voice-2 bar-rest bar ",
            "voice-1 bar-rest voice-2 bar-rest bar ",
            "voice-1 bar-rest voice-2 bar-rest bar ",
            "voice-1 bar-rest voice-2 bar-rest bar ",
            "voice-1 bar-rest voice-2 bar-rest bar"
        );
        let dec2 = decode_bekern_unmerged(multi_blank).unwrap();
        assert!(
            is_empty_score_input(&dec2),
            "all-bar-rest multi-staff page must abstain to empty ScoreInput"
        );

        let empty_xml = to_empty_musicxml("Blank");
        assert!(empty_xml.contains("<score-partwise version=\"3.1\">"));
        assert!(!empty_xml.contains("<measure"));
        assert!(!empty_xml.contains("<note"));
    }

    #[test]
    fn grand_staff_grouping_emits_staves_2_for_piano_and_voice_piano_not_quartet() {
        use super::super::{has_errors, parse_score, to_musicxml, validate_musicxml, LossTable};
        use super::decode_bekern_unmerged;

        // 1) Solo piano (G2, F4) -> 1 <part> with <staves>2</staves>, <staff>1</staff>, <staff>2</staff>, <backup>
        let piano = concat!(
            "clef-G-2 key+0 time-4-4 clef-F-4 key+0 time-4-4 ",
            "voice-1 half C 5 half E 5 voice-2 whole C 3 bar-light-heavy"
        );
        let p_dec = decode_bekern_unmerged(piano).unwrap();
        let (p_score, _) = parse_score(&p_dec).unwrap();
        let p_xml = to_musicxml(&p_score, &mut LossTable::default());
        assert!(!has_errors(&validate_musicxml(&p_xml)));
        assert_eq!(p_xml.matches("<part id=").count(), 1, "piano must emit 1 <part>");
        assert!(p_xml.contains("<staves>2</staves>"));
        assert!(p_xml.contains("<clef number=\"1\"><sign>G</sign><line>2</line></clef>"));
        assert!(p_xml.contains("<clef number=\"2\"><sign>F</sign><line>4</line></clef>"));
        assert!(p_xml.contains("<staff>1</staff>"));
        assert!(p_xml.contains("<staff>2</staff>"));
        assert!(p_xml.contains("<backup><duration>1920</duration></backup>"));

        // 2) Voice + Piano (G2, G2, F4) -> 2 <part>s: P1 (1-staff Voice) + P2 (2-staff Piano)
        let vp = concat!(
            "clef-G-2 key+0 time-4-4 clef-G-2 key+0 time-4-4 clef-F-4 key+0 time-4-4 ",
            "voice-1 whole A 4 voice-2 whole C 5 voice-3 whole C 3 bar-light-heavy"
        );
        let vp_dec = decode_bekern_unmerged(vp).unwrap();
        let (vp_score, _) = parse_score(&vp_dec).unwrap();
        let vp_xml = to_musicxml(&vp_score, &mut LossTable::default());
        assert!(!has_errors(&validate_musicxml(&vp_xml)));
        assert_eq!(vp_xml.matches("<part id=").count(), 2, "voice+piano must emit 2 <part>s");
        assert_eq!(vp_xml.matches("<staves>2</staves>").count(), 1);

        // 3) String quartet (G2, G2, C3, F4) -> 4 separate 1-staff <part>s, NO <staves>2</staves>
        let sq = concat!(
            "clef-G-2 key+0 time-4-4 clef-G-2 key+0 time-4-4 ",
            "clef-C-3 key+0 time-4-4 clef-F-4 key+0 time-4-4 ",
            "voice-1 whole E 5 voice-2 whole C 5 voice-3 whole G 4 voice-4 whole C 3 bar-light-heavy"
        );
        let sq_dec = decode_bekern_unmerged(sq).unwrap();
        let (sq_score, _) = parse_score(&sq_dec).unwrap();
        let sq_xml = to_musicxml(&sq_score, &mut LossTable::default());
        assert!(!has_errors(&validate_musicxml(&sq_xml)));
        assert_eq!(sq_xml.matches("<part id=").count(), 4, "string quartet must keep 4 separate <part>s");
        assert!(!sq_xml.contains("<staves>2</staves>"));
    }

    #[test]
    fn bekern_v2_inline_cursor_anchored_dynamics_hairpins_and_midbar_context() {
        use super::super::{has_errors, parse_score, to_musicxml, validate_musicxml, LossTable};
        use super::decode_bekern_unmerged;

        // Beat 0: dyn-p + hairpin-cresc-start; beat 2.0 (mid-bar): dyn-sf + hairpin-stop + clef-F-4 + ottava-8va;
        // beat 3.0: ottava-stop.
        let stream = concat!(
            "clef-G-2 key+0 time-4-4 ",
            "dyn-p hairpin-cresc-start voice-1 quarter C 4 quarter D 4 ",
            "clef-F-4 dyn-sf hairpin-stop ottava-8va quarter E 4 ",
            "ottava-stop quarter F 4 bar-light-heavy"
        );
        let dec = decode_bekern_unmerged(stream).unwrap();
        let m1 = dec.measure_attrs.iter().find(|a| a.measure == 1).unwrap();
        assert_eq!(m1.all_dynamics(), vec!["p", "sf"]);
        assert_eq!(m1.all_hairpins(), vec!["cresc", "stop"]);
        assert!(
            m1.timed_events
                .iter()
                .any(|e| e.kind == "dynamic" && e.value == "sf" && (e.onset_beats - 2.0).abs() < 1e-6),
            "dyn-sf must anchor at beat 2.0, got: {:?}",
            m1.timed_events
        );
        assert!(
            m1.timed_events
                .iter()
                .any(|e| e.kind == "clef" && e.value == "F-4" && (e.onset_beats - 2.0).abs() < 1e-6),
            "mid-bar clef-F-4 must anchor at beat 2.0"
        );
        assert!(
            m1.timed_events
                .iter()
                .any(|e| e.kind == "ottava" && e.value == "8va" && (e.onset_beats - 2.0).abs() < 1e-6),
            "ottava-8va must anchor at beat 2.0"
        );
        assert!(
            m1.timed_events
                .iter()
                .any(|e| e.kind == "ottava" && e.value == "stop" && (e.onset_beats - 3.0).abs() < 1e-6),
            "ottava-stop must anchor at beat 3.0"
        );

        let (score, _) = parse_score(&dec).unwrap();
        let xml = to_musicxml(&score, &mut LossTable::default());
        assert!(!has_errors(&validate_musicxml(&xml)));
        // Verify ordering in XML: <dynamics><p/> before D4, and <dynamics><sf/> AFTER D4 and BEFORE E4!
        let pos_p = xml.find("<dynamics><p/></dynamics>").unwrap();
        let pos_d4 = xml.find("<step>D</step>").unwrap();
        let pos_sf = xml.find("<dynamics><sf/></dynamics>").unwrap();
        let pos_e4 = xml.find("<step>E</step>").unwrap();
        let pos_fclef = xml.find("<clef><sign>F</sign><line>4</line></clef>").unwrap();
        let pos_8va = xml.find("<octave-shift type=\"down\" size=\"8\"/>").unwrap();
        let pos_8va_stop = xml.find("<octave-shift type=\"stop\" size=\"8\"/>").unwrap();
        let pos_f4 = xml.find("<step>F</step>").unwrap();
        assert!(pos_p < pos_d4);
        assert!(pos_d4 < pos_sf && pos_sf < pos_e4, "dyn-sf must emit mid-bar between D4 and E4");
        assert!(pos_d4 < pos_fclef && pos_fclef < pos_e4, "mid-bar clef-F-4 must emit between D4 and E4");
        assert!(pos_d4 < pos_8va && pos_8va < pos_e4);
        assert!(pos_e4 < pos_8va_stop && pos_8va_stop < pos_f4);
    }

    #[test]
    fn bekern_v2_explicit_staff_xstaff_rational_tuplets_and_all_expr_tokens_roundtrip() {
        use super::super::bekern::to_bekern_v2;
        use super::super::musicxml_import::{bekern_events, staff_headers};
        use super::super::{has_errors, parse_score, to_musicxml, validate_musicxml, LossTable};
        use super::decode_bekern_unmerged;

        // 1) Explicit <staff> + xstaff-down + tup-open 11 8 ... tup-close + all expressive/structural tokens
        let stream = concat!(
            "clef-G-2 key+0 time-4-4 clef-F-4 key+0 time-4-4 ",
            "volta-1 dyn-ppp voice-1 ",
            "quarter C 5 art-arpeggiate art-trill art-tremolo art-mordent art-turn ",
            "quarter D 5 art-staccatissimo art-up-bow art-down-bow art-soft-accent cue ",
            "tup-open 6 4 eighth E 5 eighth F 5 eighth G 5 eighth A 5 eighth B 5 eighth C 6 tup-close ",
            "<staff> dyn-rfz voice-1 half C 3 voice-2 rest half half G 3 xstaff-up bar-repeat-right ",
            "volta-2 dyn-sffz voice-1 whole C 5 <staff> dyn-fp voice-1 whole C 3 bar-light-heavy"
        );
        let dec = decode_bekern_unmerged(stream).unwrap();
        assert_eq!(dec.parts.len(), 2);
        assert_eq!(dec.parts[0].notes[0].articulations.len(), 5);
        assert!(dec.parts[0].notes[1].cue);
        assert_eq!(dec.parts[0].notes[2].tuplet, Some((6, 4)));
        assert_eq!(dec.parts[1].notes[2].xstaff, Some(-1));

        let (score, _) = parse_score(&dec).unwrap();
        let xml = to_musicxml(&score, &mut LossTable::default());
        assert!(!has_errors(&validate_musicxml(&xml)));
        for needle in [
            "<ending number=\"1\" type=\"start\"/>",
            "<ending number=\"2\" type=\"start\"/>",
            "<dynamics><ppp/></dynamics>",
            "<dynamics><rfz/></dynamics>",
            "<dynamics><sffz/></dynamics>",
            "<dynamics><fp/></dynamics>",
            "<arpeggiate/>",
            "<trill-mark/>",
            "<tremolo>3</tremolo>",
            "<inverted-mordent/>",
            "<turn/>",
            "<staccatissimo/>",
            "<up-bow/>",
            "<down-bow/>",
            "<soft-accent/>",
            "<cue/>",
            "<actual-notes>6</actual-notes><normal-notes>4</normal-notes>",
        ] {
            assert!(xml.contains(needle), "missing {needle} in rendered MusicXML");
        }

        // 2) Full MusicXML -> bekern_events -> to_bekern_v2 -> decode_bekern_unmerged -> to_musicxml round-trip
        let evs = bekern_events(&xml).unwrap();
        let hdrs = staff_headers(&xml).unwrap();
        let (toks_v2, _) = to_bekern_v2(&evs, &hdrs).unwrap();
        let joined = toks_v2.join(" ");
        for tok in [
            "<staff>",
            "volta-1",
            "volta-2",
            "dyn-ppp",
            "dyn-rfz",
            "dyn-sffz",
            "dyn-fp",
            "art-arpeggiate",
            "art-trill",
            "art-tremolo",
            "art-mordent",
            "art-turn",
            "art-staccatissimo",
            "art-up-bow",
            "art-down-bow",
            "art-soft-accent",
            "cue",
            "tup-open",
            "tup-close",
        ] {
            assert!(
                toks_v2.iter().any(|t| t == tok),
                "expected {tok} in v2 tokens: {joined}"
            );
        }
        assert!(
            joined.contains("tup-open 6 4"),
            "expected rational tup-open 6 4 in v2 tokens: {joined}"
        );
        let dec2 = decode_bekern_unmerged(&joined).unwrap();
        let (score2, _) = parse_score(&dec2).unwrap();
        let xml2 = to_musicxml(&score2, &mut LossTable::default());
        assert!(!has_errors(&validate_musicxml(&xml2)));
        assert!(xml2.contains("<actual-notes>6</actual-notes><normal-notes>4</normal-notes>"));
        assert!(xml2.contains("<arpeggiate/>"));
        assert!(xml2.contains("<dynamics><rfz/></dynamics>"));
    }

    #[test]
    fn all_treble_six_staff_dp_prefers_bottom_piano_staves_not_staff_one() {
        use super::assign_segments_to_staves;

        // 6-staff SATB+Piano all in clef-G-2 (like beach-prayer):
        // 7 segments (1 extra polyphonic voice) -> must NOT pack into Staff 1 (idx 0);
        // extra voice goes to bottom staff (idx 5), keeping staves 0..4 aligned 1-to-1.
        let six_treble = vec![('G', 2, None); 6];
        let seven_segs = vec![
            Some(74.0),
            Some(69.0),
            Some(64.0),
            Some(60.0),
            Some(72.0),
            Some(67.0),
            Some(62.0),
        ];
        let map7 = assign_segments_to_staves(&seven_segs, &six_treble, &[]);
        assert_eq!(map7[0], (0, 1), "Staff 1 stays 1-voice");
        assert_eq!(map7[1], (1, 1), "Staff 2 stays 1-voice");
        assert_eq!(map7[2], (2, 1), "Staff 3 stays 1-voice");
        assert_eq!(map7[3], (3, 1), "Staff 4 stays 1-voice");
        assert_eq!(map7[4], (4, 1), "Staff 5 stays 1-voice");
        assert_eq!(map7[5], (5, 1), "Staff 6 voice 1");
        assert_eq!(map7[6], (5, 2), "Staff 6 voice 2");

        // 8 segments (2 extra polyphonic voices) -> spread across Piano RH (idx 4) & LH (idx 5),
        // leaving SATB staves 0..3 strictly 1-voice each.
        let eight_segs = vec![
            Some(74.0),
            Some(69.0),
            Some(64.0),
            Some(60.0),
            Some(72.0),
            Some(67.0),
            Some(62.0),
            Some(57.0),
        ];
        let map8 = assign_segments_to_staves(&eight_segs, &six_treble, &[]);
        assert_eq!(map8[0], (0, 1));
        assert_eq!(map8[1], (1, 1));
        assert_eq!(map8[2], (2, 1));
        assert_eq!(map8[3], (3, 1));
        assert_eq!(map8[4], (4, 1));
        assert_eq!(map8[5], (4, 2));
        assert_eq!(map8[6], (5, 1));
        assert_eq!(map8[7], (5, 2));

        // 3-staff Voice+Piano (G-2, G-2, F-4, like schumann-dichterliebe-2) with 4 voices:
        // extra treble voice belongs on Staff 2 (Piano RH, idx 1), not Staff 1 (Voice, idx 0).
        let vp_clefs = vec![('G', 2, None), ('G', 2, None), ('F', 4, None)];
        let vp_segs = vec![Some(72.0), Some(71.0), Some(68.0), Some(48.0)];
        let vp_map = assign_segments_to_staves(&vp_segs, &vp_clefs, &[]);
        assert_eq!(vp_map, vec![(0, 1), (1, 1), (1, 2), (2, 1)]);
    }

    #[test]
    fn per_staff_transposing_key_signatures_roundtrip() {
        use super::super::{
            has_errors, parse_score, to_mscx, to_musicxml, validate_musicxml, LossTable,
        };
        use super::decode_bekern_unmerged;

        // Transposing score (Weber Concertino): Clarinet in Bb (key-1) + Piano RH/LH (key-3).
        let stream = concat!(
            "clef-G-2 key-1 time-4-4 ",
            "clef-G-2 key-3 time-4-4 ",
            "clef-F-4 key-3 time-4-4 ",
            "voice-1 whole D 5 ",
            "voice-2 whole C 5 ",
            "voice-3 whole C 3 bar"
        );
        let dec = decode_bekern_unmerged(stream).unwrap();
        assert_eq!(dec.parts.len(), 3);
        assert_eq!(dec.parts[0].key_fifths, Some(-1));
        assert_eq!(dec.parts[1].key_fifths, Some(-3));
        assert_eq!(dec.parts[2].key_fifths, Some(-3));

        let (score, _) = parse_score(&dec).unwrap();
        let xml = to_musicxml(&score, &mut LossTable::default());
        assert!(!has_errors(&validate_musicxml(&xml)));
        let p1_start = xml.find("<part id=\"P1\">").expect("P1");
        let p2_start = xml.find("<part id=\"P2\">").expect("P2");
        let p1_xml = &xml[p1_start..p2_start];
        let p2_xml = &xml[p2_start..];
        assert!(
            p1_xml.contains("<key><fifths>-1</fifths></key>"),
            "P1 (Clarinet) must have key-1: {p1_xml}"
        );
        assert!(
            p2_xml.contains("<key><fifths>-3</fifths></key>"),
            "P2 (Piano) must have key-3: {p2_xml}"
        );

        let mscx = to_mscx(&score, &mut LossTable::default());
        assert!(mscx.contains("<concertKey>-1</concertKey>"));
        assert!(mscx.contains("<concertKey>-3</concertKey>"));
    }

    #[test]
    fn sparse_secondary_polyphonic_voice_uses_forward_not_visible_rest() {
        use super::super::{has_errors, parse_score, to_musicxml, validate_musicxml, LossTable};
        use super::decode_bekern_unmerged;

        // 3-measure score:
        // - m1: voice-1 only (no voice-2)
        // - m2: voice-1 + voice-2 with explicit `rest quarter`, `quarter E 4`, and implicit trailing gap (beats 3..4)
        // - m3: voice-1 only (no voice-2)
        let stream = concat!(
            "clef-G-2 key+0 time-4-4 ",
            "voice-1 whole C 5 bar ",
            "voice-1 whole D 5 voice-2 rest quarter quarter E 4 bar ",
            "voice-1 whole F 5 bar"
        );
        let dec = decode_bekern_unmerged(stream).unwrap();
        let (score, _) = parse_score(&dec).unwrap();
        let xml = to_musicxml(&score, &mut LossTable::default());
        assert!(
            !has_errors(&validate_musicxml(&xml)),
            "MusicXML must validate cleanly: {xml}"
        );

        // Only 1 visible <rest/> across the whole 3-measure score (the explicit `rest quarter` in m2).
        let rest_count = xml.matches("<rest/>").count();
        assert_eq!(
            rest_count, 1,
            "expected only the 1 explicit rest, not spurious secondary-voice padding: {xml}"
        );
        // Measure 2 pads voice 2's implicit trailing half-measure gap with <forward> (960 ticks).
        assert!(
            xml.contains("<forward><duration>960</duration><voice>2</voice></forward>"),
            "expected <forward> padding for sparse secondary voice: {xml}"
        );
        // Only measure 2 should contain a <backup>.
        assert_eq!(
            xml.matches("<backup>").count(),
            1,
            "measures 1 and 3 must omit empty secondary voice: {xml}"
        );
    }

    #[test]
    fn bekern_v2_emits_all_28_phase_a_tokens() {
        use super::super::bekern::to_bekern_v2;
        use super::super::musicxml_import::{bekern_events, staff_headers};
        use super::super::{has_errors, parse_score, to_musicxml, validate_musicxml, LossTable};
        use super::decode_bekern_unmerged;

        // Build a valid MusicXML containing every single one of the 28 bekern+v2 Phase A tokens.
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<score-partwise version="3.1">
  <part-list><score-part id="P1"><part-name>Piano</part-name></score-part></part-list>
  <part id="P1">
    <measure number="1">
      <attributes>
        <divisions>480</divisions>
        <key><fifths>0</fifths></key>
        <time><beats>4</beats><beat-type>4</beat-type></time>
        <staves>2</staves>
        <clef number="1"><sign>G</sign><line>2</line></clef>
        <clef number="2"><sign>F</sign><line>4</line></clef>
      </attributes>
      <barline location="left"><ending number="1" type="start"/></barline>
      <direction><direction-type><dynamics><ppp/></dynamics></direction-type><staff>1</staff></direction>
      <direction><direction-type><octave-shift type="down" size="8"/></direction-type><staff>1</staff></direction>
      <note>
        <pitch><step>C</step><octave>5</octave></pitch>
        <duration>480</duration><voice>1</voice><type>quarter</type><staff>1</staff>
        <notations>
          <arpeggiate/>
          <ornaments><trill-mark/><tremolo>3</tremolo><inverted-mordent/><turn/></ornaments>
          <articulations><staccatissimo/><soft-accent/></articulations>
          <technical><up-bow/><down-bow/></technical>
        </notations>
      </note>
      <direction><direction-type><dynamics><fff/></dynamics></direction-type><staff>1</staff></direction>
      <direction><direction-type><octave-shift type="stop" size="8"/></direction-type><staff>1</staff></direction>
      <note>
        <cue/>
        <pitch><step>D</step><octave>5</octave></pitch>
        <duration>480</duration><voice>1</voice><type>quarter</type><staff>1</staff>
      </note>
      <direction><direction-type><dynamics><fp/></dynamics></direction-type><staff>1</staff></direction>
      <direction><direction-type><octave-shift type="up" size="8"/></direction-type><staff>1</staff></direction>
      <note>
        <pitch><step>E</step><octave>5</octave></pitch>
        <duration>320</duration><voice>1</voice><type>quarter</type>
        <time-modification><actual-notes>3</actual-notes><normal-notes>2</normal-notes></time-modification>
        <staff>1</staff>
        <notations><tuplet type="start" number="1"/></notations>
      </note>
      <note>
        <pitch><step>F</step><octave>5</octave></pitch>
        <duration>320</duration><voice>1</voice><type>quarter</type>
        <time-modification><actual-notes>3</actual-notes><normal-notes>2</normal-notes></time-modification>
        <staff>1</staff>
      </note>
      <note>
        <pitch><step>G</step><octave>5</octave></pitch>
        <duration>320</duration><voice>1</voice><type>quarter</type>
        <time-modification><actual-notes>3</actual-notes><normal-notes>2</normal-notes></time-modification>
        <staff>1</staff>
        <notations><tuplet type="stop" number="1"/></notations>
      </note>
      <backup><duration>1920</duration></backup>
      <direction><direction-type><dynamics><sf/></dynamics></direction-type><staff>2</staff></direction>
      <note>
        <pitch><step>C</step><octave>3</octave></pitch>
        <duration>960</duration><voice>5</voice><type>half</type><staff>2</staff>
      </note>
      <direction><direction-type><dynamics><sfp/></dynamics></direction-type><staff>2</staff></direction>
      <note>
        <pitch><step>G</step><octave>3</octave></pitch>
        <duration>960</duration><voice>5</voice><type>half</type><staff>1</staff>
      </note>
    </measure>
    <measure number="2">
      <barline location="left"><ending number="2" type="start"/></barline>
      <direction><direction-type><dynamics><fz/></dynamics></direction-type><staff>1</staff></direction>
      <note>
        <pitch><step>C</step><octave>5</octave></pitch>
        <duration>960</duration><voice>1</voice><type>half</type><staff>1</staff>
      </note>
      <direction><direction-type><dynamics><rfz/></dynamics></direction-type><staff>1</staff></direction>
      <note>
        <pitch><step>B</step><octave>4</octave></pitch>
        <duration>960</duration><voice>1</voice><type>half</type><staff>2</staff>
      </note>
      <backup><duration>1920</duration></backup>
      <direction><direction-type><dynamics><sffz/></dynamics></direction-type><staff>2</staff></direction>
      <note>
        <pitch><step>C</step><octave>3</octave></pitch>
        <duration>1920</duration><voice>5</voice><type>whole</type><staff>2</staff>
      </note>
    </measure>
  </part>
</score-partwise>"#;

        let evs = bekern_events(xml).unwrap();
        let hdrs = staff_headers(xml).unwrap();
        let (toks_v2, _) = to_bekern_v2(&evs, &hdrs).unwrap();
        let joined = toks_v2.join(" ");

        let phase_a_28 = [
            // 8 extended dynamics
            "dyn-ppp",
            "dyn-fff",
            "dyn-fp",
            "dyn-sf",
            "dyn-sfp",
            "dyn-fz",
            "dyn-rfz",
            "dyn-sffz",
            // 9 articulations / ornaments / technical
            "art-arpeggiate",
            "art-trill",
            "art-tremolo",
            "art-mordent",
            "art-turn",
            "art-staccatissimo",
            "art-up-bow",
            "art-down-bow",
            "art-soft-accent",
            // 11 structural markers
            "<staff>",
            "xstaff-up",
            "xstaff-down",
            "ottava-8va",
            "ottava-8vb",
            "ottava-stop",
            "cue",
            "volta-1",
            "volta-2",
            "tup-open",
            "tup-close",
        ];
        assert_eq!(phase_a_28.len(), 28);
        for tok in phase_a_28 {
            assert!(
                toks_v2.iter().any(|t| t == tok),
                "missing Phase A token {tok:?} in v2 output: {joined}"
            );
        }

        // Round-trip decode -> MusicXML must validate with zero errors.
        let dec = decode_bekern_unmerged(&joined).unwrap();
        let (score, _) = parse_score(&dec).unwrap();
        let out_xml = to_musicxml(&score, &mut LossTable::default());
        assert!(
            !has_errors(&validate_musicxml(&out_xml)),
            "v2 round-trip MusicXML failed validation"
        );
    }
}
