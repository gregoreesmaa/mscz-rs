//! Native MuseScore 3/4 `.mscx` XML -> [`ScoreInput`] importer.
//!
//! Std-only XML parser (matching [`crate::musicxml_import`] style) extracting:
//! - `<Part>` / `<Staff>` structure and part names
//! - `<Measure>` and `<voice>` layout with repeat barlines and `<BarLine>`
//! - `<Clef>`, `<KeySig>`, `<TimeSig>` (opening and mid-stream)
//! - `<Chord>` / `<Note>` / `<Rest>` (MIDI `<pitch>`, line-of-fifths `<tpc>`,
//!   `<durationType>`, `<dots>`, `<duration>`, `<Tuplet>`/`<endTuplet/>`,
//!   `<Spanner type="Tie">`, `<Spanner type="Slur">`, `<Articulation>`,
//!   `<Fermata>`, `<small>`, `<staffMove>`, grace chords)
//! - `<Dynamic>`, `<Lyrics>`, `<Harmony>`, `<Tempo>`,
//!   `<Spanner type="HairPin">`, `<Spanner type="Ottava">`, and `<Spanner type="Volta">`

use crate::{MeasureAttr, NoteInput, PartInput, ScoreInput, TimeSig, TimedEvent, SCORE_DIVISIONS};

/// Convert a MuseScore line-of-fifths Tonal Pitch Class (`C=14`, `G=15`,
/// `F=13`, `F#=20`, `Bb=12`, ...) into `(step, alter)`.
pub fn tpc_to_step_alter(tpc: i32) -> (char, i32) {
    let offset = tpc - 13;
    let step = match offset.rem_euclid(7) {
        0 => 'F',
        1 => 'C',
        2 => 'G',
        3 => 'D',
        4 => 'A',
        5 => 'E',
        _ => 'B',
    };
    let alter = offset.div_euclid(7);
    (step, alter)
}

fn step_semitone(step: char) -> i32 {
    match step {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => 0,
    }
}

fn alter_str(alter: i32) -> &'static str {
    match alter {
        -2 => "bb",
        -1 => "b",
        1 => "#",
        2 => "##",
        _ => "",
    }
}

/// Reconstruct a scientific pitch string (`"C4"`, `"F#5"`, `"Bb3"`) from
/// MuseScore MIDI `<pitch>` and optional `<tpc>` (falling back to key-aware
/// spelling when `<tpc>` is omitted).
fn mscx_pitch_string(midi: i32, tpc_opt: Option<i32>, key_fifths: i32) -> String {
    let (step, alter, octave) = if let Some(tpc) = tpc_opt {
        let (s, a) = tpc_to_step_alter(tpc);
        let oct = (midi - step_semitone(s) - a).div_euclid(12) - 1;
        (s, a.clamp(-2, 2), oct)
    } else {
        crate::pitch::midi_to_pitch_for_key(midi.clamp(0, 127) as u8, key_fifths)
    };
    format!("{step}{}{octave}", alter_str(alter))
}

fn unescape_xml(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Decode a MuseScore 3/4 clef type string (`"G"`, `"F"`, `"G8vb"`, `"C3"`, ...)
/// into MusicXML-style `(sign, line, octave_change)`.
fn parse_mscx_clef(ct: &str) -> (char, u32, Option<i32>) {
    match ct.trim() {
        "G" | "G2" => ('G', 2, None),
        "G8vb" => ('G', 2, Some(-1)),
        "G8va" => ('G', 2, Some(1)),
        "G15mb" => ('G', 2, Some(-2)),
        "G15ma" => ('G', 2, Some(2)),
        "G1" => ('G', 1, None),
        "F" | "F4" => ('F', 4, None),
        "F8vb" => ('F', 4, Some(-1)),
        "F8va" => ('F', 4, Some(1)),
        "F3" => ('F', 3, None),
        "F5" => ('F', 5, None),
        "C1" => ('C', 1, None),
        "C2" => ('C', 2, None),
        "C3" => ('C', 3, None),
        "C4" => ('C', 4, None),
        "C5" => ('C', 5, None),
        "PERC" | "PERC2" | "TAB" => ('p', 2, None),
        _ => ('G', 2, None),
    }
}

fn duration_type_beats(dtype: &str) -> Option<f64> {
    match dtype.trim() {
        "long" => Some(16.0),
        "breve" => Some(8.0),
        "whole" => Some(4.0),
        "half" => Some(2.0),
        "quarter" => Some(1.0),
        "eighth" => Some(0.5),
        "16th" => Some(0.25),
        "32nd" => Some(0.125),
        "64th" => Some(0.0625),
        "128th" => Some(0.03125),
        "256th" => Some(0.015625),
        _ => None,
    }
}

/// Parse a MuseScore fraction string `"N/D"` into beats (`4.0 * N / D`).
fn parse_fraction_beats(s: &str) -> Option<f64> {
    let (num_s, den_s) = s.trim().split_once('/')?;
    let num: f64 = num_s.trim().parse().ok()?;
    let den: f64 = den_s.trim().parse().ok()?;
    if den.abs() < 1e-12 {
        return None;
    }
    Some(4.0 * num / den)
}

// ---------------------------------------------------------------------------
// Minimal std-only XML element walker (handles nested same-name tags cleanly)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct XmlChild<'a> {
    name: &'a str,
    attrs: &'a str,
    /// Inner content between `<tag ...>` and `</tag>`, or `""` for self-closing `<tag/>`.
    inner: &'a str,
}

fn is_name_delim(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n' | b'>' | b'/')
}

/// Find the matching closing tag `</name>` for an element whose body starts at `body_start`,
/// tracking nested `<name ...>` occurrences so `<Spanner>` or `<Staff>` nesting never breaks.
fn find_matching_close(s: &str, name: &str, body_start: usize) -> Option<(usize, usize)> {
    let bytes = s.as_bytes();
    let nlen = name.len();
    let mut depth = 1usize;
    let mut pos = body_start;
    while pos < bytes.len() {
        let rel = s[pos..].find('<')?;
        let lt = pos + rel;
        if s[lt..].starts_with("<!--") {
            let end_c = s[lt + 4..].find("-->")?;
            pos = lt + 4 + end_c + 3;
            continue;
        }
        if s[lt..].starts_with("</") {
            let after = lt + 2;
            if s[after..].starts_with(name) {
                let end_name = after + nlen;
                let rest = &s[end_name..];
                if let Some(gt) = rest.find('>') {
                    if rest[..gt].trim().is_empty() {
                        depth -= 1;
                        if depth == 0 {
                            return Some((lt, end_name + gt + 1));
                        }
                        pos = end_name + gt + 1;
                        continue;
                    }
                }
            }
            pos = after;
            continue;
        }
        // Opening tag
        let after = lt + 1;
        if s[after..].starts_with(name) {
            let end_name = after + nlen;
            if end_name < bytes.len() && is_name_delim(bytes[end_name]) {
                let gt_rel = s[end_name..].find('>')?;
                let gt = end_name + gt_rel;
                let self_closing = s[end_name..gt].trim_end().ends_with('/');
                if !self_closing {
                    depth += 1;
                }
                pos = gt + 1;
                continue;
            }
        }
        pos = after;
    }
    None
}

/// Iterate direct child elements of an XML body slice in document order.
fn direct_children(body: &str) -> Vec<XmlChild<'_>> {
    let mut out = Vec::new();
    let bytes = body.as_bytes();
    let mut pos = 0usize;
    while pos < bytes.len() {
        let Some(rel) = body[pos..].find('<') else {
            break;
        };
        let lt = pos + rel;
        let rem = &body[lt..];
        if rem.starts_with("<!--") {
            if let Some(ec) = rem[4..].find("-->") {
                pos = lt + 4 + ec + 3;
                continue;
            }
            break;
        }
        if rem.starts_with("<?") {
            if let Some(ec) = rem[2..].find("?>") {
                pos = lt + 2 + ec + 2;
                continue;
            }
            break;
        }
        if rem.starts_with("<!") {
            if let Some(gt) = rem.find('>') {
                pos = lt + gt + 1;
                continue;
            }
            break;
        }
        if rem.starts_with("</") {
            if let Some(gt) = rem.find('>') {
                pos = lt + gt + 1;
                continue;
            }
            break;
        }
        let name_start = lt + 1;
        let mut name_end = name_start;
        while name_end < bytes.len() && !is_name_delim(bytes[name_end]) {
            name_end += 1;
        }
        if name_end == name_start {
            pos = name_start + 1;
            continue;
        }
        let name = &body[name_start..name_end];
        let Some(gt_rel) = body[name_end..].find('>') else {
            break;
        };
        let gt = name_end + gt_rel;
        let raw_attrs = &body[name_end..gt];
        if let Some(trimmed) = raw_attrs.trim_end().strip_suffix('/') {
            out.push(XmlChild {
                name,
                attrs: trimmed.trim(),
                inner: "",
            });
            pos = gt + 1;
        } else {
            let body_start = gt + 1;
            if let Some((close_lt, close_end)) = find_matching_close(body, name, body_start) {
                out.push(XmlChild {
                    name,
                    attrs: raw_attrs.trim(),
                    inner: &body[body_start..close_lt],
                });
                pos = close_end;
            } else {
                pos = body_start;
            }
        }
    }
    out
}

fn attr_value<'a>(attrs: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("{key}=\"");
    let pos = attrs.find(&needle)?;
    let start = pos + needle.len();
    let end = attrs[start..].find('"')? + start;
    Some(&attrs[start..end])
}

fn child_text(body: &str, tag: &str) -> Option<String> {
    direct_children(body)
        .into_iter()
        .find(|c| c.name == tag)
        .map(|c| unescape_xml(c.inner.trim()))
}

fn has_child(body: &str, tag: &str) -> bool {
    direct_children(body).into_iter().any(|c| c.name == tag)
}

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

/// Strip simple HTML/rich-text tags inside MuseScore `<text>` elements
/// (e.g. `<sym>metNoteQuarterUp</sym> = 120` -> `= 120` or `<i>Allegro</i>` -> `Allegro`).
fn strip_inline_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    unescape_xml(out.trim())
}

/// Map a MuseScore `<Articulation><subtype>` string to a canonical `bekern+`
/// articulation name.
fn parse_mscx_articulation(sub: &str) -> Option<&'static str> {
    let s = sub.trim();
    if s.contains("Staccatissimo") {
        Some("art-staccatissimo")
    } else if s.contains("Staccato") {
        Some("stacc")
    } else if s.contains("SoftAccent") {
        Some("art-soft-accent")
    } else if s.contains("Accent") || s.contains("Marcato") {
        Some("accent")
    } else if s.contains("Tenuto") {
        Some("tenuto")
    } else if s.contains("UpBow") {
        Some("art-up-bow")
    } else if s.contains("DownBow") {
        Some("art-down-bow")
    } else if s.contains("Mordent") {
        Some("art-mordent")
    } else if s.contains("Turn") {
        Some("art-turn")
    } else {
        None
    }
}

/// Format a MuseScore `<Harmony>` element (`<root>`, optional `<name>`, optional `<base>`)
/// into a readable chord symbol such as `"C"`, `"Am7"`, or `"G/B"`.
fn parse_harmony_symbol(inner: &str) -> Option<String> {
    let root_tpc: i32 = child_text(inner, "root")?.parse().ok()?;
    let (r_step, r_alter) = tpc_to_step_alter(root_tpc);
    let mut sym = format!("{r_step}{}", alter_str(r_alter));
    if let Some(name) = child_text(inner, "name") {
        if !name.is_empty() {
            sym.push_str(&name);
        }
    }
    if let Some(base_tpc) = child_text(inner, "base").and_then(|s| s.parse::<i32>().ok()) {
        let (b_step, b_alter) = tpc_to_step_alter(base_tpc);
        sym.push('/');
        sym.push(b_step);
        sym.push_str(alter_str(b_alter));
    }
    Some(sym)
}

/// Import a native MuseScore 3 or 4 `.mscx` XML document into a [`ScoreInput`].
///
/// Extracts `<Part>`/`<Staff>` structure, `<Measure>`/`<voice>` timelines,
/// `<Clef>`, `<KeySig>`, `<TimeSig>`, `<Chord>`/`<Note>`/`<Rest>` (pitch/TPC,
/// duration, dots, tuplets, ties, slurs, articulations, fermatas, cue `<small>`,
/// cross-staff `<staffMove>`), `<Dynamic>`, `<Lyrics>`, `<Harmony>`, `<Tempo>`,
/// `<Spanner type="HairPin">`, `<Spanner type="Ottava">`, and `<Spanner type="Volta">`.
pub fn import_mscx(xml: &str) -> Result<ScoreInput, String> {
    if !xml.contains("<museScore") {
        return Err("not a MuseScore .mscx document (missing <museScore>)".to_string());
    }
    let root_children = direct_children(xml);
    let ms_elem = root_children
        .iter()
        .find(|c| c.name == "museScore")
        .ok_or_else(|| "missing <museScore> root element".to_string())?;
    let score_elem = direct_children(ms_elem.inner)
        .into_iter()
        .find(|c| c.name == "Score")
        .ok_or_else(|| "missing <Score> element".to_string())?;

    let score_children = direct_children(score_elem.inner);

    let divisions: u32 = score_children
        .iter()
        .find(|c| c.name == "Division")
        .and_then(|c| c.inner.trim().parse().ok())
        .filter(|&d| d > 0)
        .unwrap_or(SCORE_DIVISIONS);

    let mut title: Option<String> = None;
    let mut composer: Option<String> = None;
    let mut staff_names: Vec<String> = Vec::new();
    let mut content_staves: Vec<XmlChild<'_>> = Vec::new();

    for ch in &score_children {
        match ch.name {
            "metaTag" => {
                let name = attr_value(ch.attrs, "name").unwrap_or("");
                let val = unescape_xml(ch.inner.trim());
                if !val.is_empty() {
                    if name == "workTitle" && title.is_none() {
                        title = Some(val);
                    } else if name == "composer" && composer.is_none() {
                        composer = Some(val);
                    }
                }
            }
            "Part" => {
                let p_children = direct_children(ch.inner);
                let track_name = p_children
                    .iter()
                    .find(|c| c.name == "trackName")
                    .map(|c| unescape_xml(c.inner.trim()))
                    .filter(|s| !s.is_empty())
                    .or_else(|| {
                        p_children
                            .iter()
                            .find(|c| c.name == "Instrument")
                            .and_then(|inst| {
                                child_text(inst.inner, "longName")
                                    .or_else(|| child_text(inst.inner, "trackName"))
                            })
                            .filter(|s| !s.is_empty())
                    })
                    .unwrap_or_else(|| format!("Part {}", staff_names.len() + 1));
                let n_staves = p_children
                    .iter()
                    .filter(|c| c.name == "Staff")
                    .count()
                    .max(1);
                for _ in 0..n_staves {
                    staff_names.push(track_name.clone());
                }
            }
            "Staff" => {
                content_staves.push(*ch);
            }
            _ => {}
        }
    }

    if content_staves.is_empty() {
        return Err("no <Staff> elements found in <Score>".to_string());
    }

    let mut score_time: Option<TimeSig> = None;
    let mut score_key_fifths: Option<i32> = None;
    let mut parts: Vec<PartInput> = Vec::with_capacity(content_staves.len());

    for (st_idx, staff) in content_staves.iter().enumerate() {
        let part_name = staff_names
            .get(st_idx)
            .cloned()
            .unwrap_or_else(|| format!("Staff {}", st_idx + 1));

        let mut part_clef: Option<(char, u32, Option<i32>)> = None;
        let mut part_key: Option<i32> = None;
        let mut cur_key: i32 = score_key_fifths.unwrap_or(0);
        let mut cur_time = score_time.unwrap_or(TimeSig {
            beats: 4,
            beat_type: 4,
        });
        let mut notes: Vec<NoteInput> = Vec::new();
        let mut p_measure_attrs: Vec<MeasureAttr> = Vec::new();

        let staff_children = direct_children(staff.inner);
        // Fallback title/composer from VBox if metaTag was absent.
        if title.is_none() || composer.is_none() {
            for ch in &staff_children {
                if ch.name == "VBox" {
                    for tb in direct_children(ch.inner) {
                        if tb.name == "Text" {
                            let style = child_text(tb.inner, "style").unwrap_or_default();
                            let txt = child_text(tb.inner, "text")
                                .map(|t| strip_inline_tags(&t))
                                .unwrap_or_default();
                            if !txt.is_empty() {
                                if style == "title" && title.is_none() {
                                    title = Some(txt);
                                } else if style == "composer" && composer.is_none() {
                                    composer = Some(txt);
                                }
                            }
                        }
                    }
                }
            }
        }

        let mut bar_start = 0.0f64;
        let mut m_idx = 0u32;

        for ch in &staff_children {
            if ch.name != "Measure" {
                continue;
            }
            m_idx += 1;
            let meas_children = direct_children(ch.inner);

            // Check repeat markers directly under <Measure>.
            for mc in &meas_children {
                if mc.name == "startRepeat" {
                    attr_for(&mut p_measure_attrs, m_idx).barline_left =
                        Some("repeat-left".to_string());
                } else if mc.name == "endRepeat" {
                    attr_for(&mut p_measure_attrs, m_idx).barline_right =
                        Some("repeat-right".to_string());
                }
            }

            // Pre-scan voice 0 for a TimeSig so `barlen` is known for measure rests.
            if let Some(first_voice) = meas_children.iter().find(|c| c.name == "voice") {
                for vc in direct_children(first_voice.inner) {
                    if vc.name == "TimeSig" {
                        if let (Some(n), Some(d)) = (
                            child_text(vc.inner, "sigN").and_then(|s| s.parse::<u32>().ok()),
                            child_text(vc.inner, "sigD").and_then(|s| s.parse::<u32>().ok()),
                        ) {
                            if n > 0 && d > 0 {
                                cur_time = TimeSig {
                                    beats: n,
                                    beat_type: d,
                                };
                                if score_time.is_none() && m_idx == 1 {
                                    score_time = Some(cur_time);
                                } else if m_idx > 1 {
                                    attr_for(&mut p_measure_attrs, m_idx).time = Some(cur_time);
                                }
                            }
                        }
                    }
                }
            }

            let barlen = cur_time.beats as f64 * 4.0 / cur_time.beat_type.max(1) as f64;
            let mut voice_num = 0u32;
            let mut grace_seq = 0u32;

            for mc in &meas_children {
                if mc.name != "voice" {
                    continue;
                }
                voice_num += 1;
                let mut cursor = 0.0f64;
                let mut tuplet_stack: Vec<(u32, u32)> = Vec::new();
                let mut pending_fermata = false;

                for vc in direct_children(mc.inner) {
                    match vc.name {
                        "Clef" => {
                            let ct = child_text(vc.inner, "concertClefType")
                                .or_else(|| child_text(vc.inner, "clefType"))
                                .unwrap_or_else(|| "G".to_string());
                            let (sign, line, oct) = parse_mscx_clef(&ct);
                            if m_idx == 1 && cursor < 1e-6 && part_clef.is_none() {
                                part_clef = Some((sign, line, oct));
                            } else if cursor > 1e-6 {
                                let val = match oct {
                                    Some(-1) => format!("{sign}-{line}-8vb"),
                                    Some(1) => format!("{sign}-{line}-8va"),
                                    _ => format!("{sign}-{line}"),
                                };
                                attr_for(&mut p_measure_attrs, m_idx)
                                    .timed_events
                                    .push(TimedEvent {
                                        onset_beats: cursor,
                                        kind: "clef".to_string(),
                                        value: val,
                                    });
                            } else {
                                let a = attr_for(&mut p_measure_attrs, m_idx);
                                a.clef_sign = Some(sign);
                                a.clef_line = Some(line);
                                a.clef_octave_change = oct;
                            }
                        }
                        "KeySig" => {
                            let fifths_opt = child_text(vc.inner, "concertKey")
                                .or_else(|| child_text(vc.inner, "accidental"))
                                .and_then(|s| s.parse::<i32>().ok());
                            if let Some(fifths) = fifths_opt {
                                cur_key = fifths;
                                if m_idx == 1 && cursor < 1e-6 && part_key.is_none() {
                                    part_key = Some(fifths);
                                    if score_key_fifths.is_none() {
                                        score_key_fifths = Some(fifths);
                                    }
                                } else if cursor > 1e-6 {
                                    attr_for(&mut p_measure_attrs, m_idx)
                                        .timed_events
                                        .push(TimedEvent {
                                            onset_beats: cursor,
                                            kind: "key".to_string(),
                                            value: fifths.to_string(),
                                        });
                                } else {
                                    attr_for(&mut p_measure_attrs, m_idx).key_fifths = Some(fifths);
                                }
                            }
                        }
                        "TimeSig" => {
                            // Already handled for m_idx==1 and m_idx>1 at bar start above;
                            // if mid-bar, record timed event.
                            if cursor > 1e-6 {
                                if let (Some(n), Some(d)) = (
                                    child_text(vc.inner, "sigN")
                                        .and_then(|s| s.parse::<u32>().ok()),
                                    child_text(vc.inner, "sigD")
                                        .and_then(|s| s.parse::<u32>().ok()),
                                ) {
                                    if n > 0 && d > 0 {
                                        attr_for(&mut p_measure_attrs, m_idx)
                                            .timed_events
                                            .push(TimedEvent {
                                                onset_beats: cursor,
                                                kind: "time".to_string(),
                                                value: format!("{n}-{d}"),
                                            });
                                    }
                                }
                            }
                        }
                        "Dynamic" => {
                            if let Some(sub) = child_text(vc.inner, "subtype") {
                                if !sub.is_empty() {
                                    let a = attr_for(&mut p_measure_attrs, m_idx);
                                    a.push_dynamic(&sub);
                                    a.timed_events.push(TimedEvent {
                                        onset_beats: cursor,
                                        kind: "dynamic".to_string(),
                                        value: sub,
                                    });
                                }
                            }
                        }
                        "Harmony" => {
                            if let Some(sym) = parse_harmony_symbol(vc.inner) {
                                attr_for(&mut p_measure_attrs, m_idx)
                                    .timed_events
                                    .push(TimedEvent {
                                        onset_beats: cursor,
                                        kind: "harmony".to_string(),
                                        value: sym,
                                    });
                            }
                        }
                        "Tempo" => {
                            let bpm_opt = child_text(vc.inner, "tempo")
                                .and_then(|s| s.parse::<f64>().ok())
                                .filter(|&v| v > 0.0)
                                .map(|bps| (bps * 60.0).round() as u32);
                            let txt_opt = child_text(vc.inner, "text")
                                .map(|t| strip_inline_tags(&t))
                                .filter(|s| !s.is_empty());
                            let val = match (txt_opt, bpm_opt) {
                                (Some(t), Some(bpm)) => format!("{t} ({bpm})"),
                                (Some(t), None) => t,
                                (None, Some(bpm)) => bpm.to_string(),
                                (None, None) => String::new(),
                            };
                            if !val.is_empty() {
                                attr_for(&mut p_measure_attrs, m_idx)
                                    .timed_events
                                    .push(TimedEvent {
                                        onset_beats: cursor,
                                        kind: "tempo".to_string(),
                                        value: val,
                                    });
                            }
                        }
                        "Spanner" => {
                            let stype = attr_value(vc.attrs, "type").unwrap_or("");
                            let sp_children = direct_children(vc.inner);
                            let has_next = sp_children.iter().any(|c| c.name == "next");
                            let has_prev = sp_children.iter().any(|c| c.name == "prev");
                            match stype {
                                "HairPin" => {
                                    if has_next {
                                        let sub = sp_children
                                            .iter()
                                            .find(|c| c.name == "HairPin")
                                            .and_then(|h| child_text(h.inner, "subtype"))
                                            .unwrap_or_else(|| "0".to_string());
                                        let hp = if sub == "1" { "dim" } else { "cresc" };
                                        let a = attr_for(&mut p_measure_attrs, m_idx);
                                        a.push_hairpin(hp);
                                        a.timed_events.push(TimedEvent {
                                            onset_beats: cursor,
                                            kind: "hairpin".to_string(),
                                            value: hp.to_string(),
                                        });
                                    } else if has_prev {
                                        let a = attr_for(&mut p_measure_attrs, m_idx);
                                        a.push_hairpin("stop");
                                        a.timed_events.push(TimedEvent {
                                            onset_beats: cursor,
                                            kind: "hairpin".to_string(),
                                            value: "stop".to_string(),
                                        });
                                    }
                                }
                                "Ottava" => {
                                    if has_next {
                                        let sub = sp_children
                                            .iter()
                                            .find(|c| c.name == "Ottava")
                                            .and_then(|o| child_text(o.inner, "subtype"))
                                            .unwrap_or_else(|| "8va".to_string());
                                        attr_for(&mut p_measure_attrs, m_idx)
                                            .timed_events
                                            .push(TimedEvent {
                                                onset_beats: cursor,
                                                kind: "ottava".to_string(),
                                                value: sub,
                                            });
                                    } else if has_prev {
                                        attr_for(&mut p_measure_attrs, m_idx)
                                            .timed_events
                                            .push(TimedEvent {
                                                onset_beats: cursor,
                                                kind: "ottava".to_string(),
                                                value: "stop".to_string(),
                                            });
                                    }
                                }
                                "Volta" => {
                                    if has_next {
                                        let ending = sp_children
                                            .iter()
                                            .find(|c| c.name == "Volta")
                                            .and_then(|v| {
                                                child_text(v.inner, "endings").or_else(|| {
                                                    child_text(v.inner, "beginText")
                                                        .map(|t| t.trim_end_matches('.').to_string())
                                                })
                                            })
                                            .unwrap_or_else(|| "1".to_string());
                                        attr_for(&mut p_measure_attrs, m_idx).volta = Some(ending);
                                    }
                                }
                                _ => {}
                            }
                        }
                        "BarLine" => {
                            if let Some(sub) = child_text(vc.inner, "subtype") {
                                let mapped = match sub.as_str() {
                                    "end" => Some("light-heavy"),
                                    "double" => Some("double"),
                                    _ => None,
                                };
                                if let Some(b) = mapped {
                                    attr_for(&mut p_measure_attrs, m_idx).barline_right =
                                        Some(b.to_string());
                                }
                            }
                        }
                        "location" => {
                            if let Some(frac) = child_text(vc.inner, "fractions")
                                .and_then(|s| parse_fraction_beats(&s))
                            {
                                cursor = (cursor + frac).max(0.0);
                            }
                        }
                        "Tuplet" => {
                            let actual = child_text(vc.inner, "actualNotes")
                                .and_then(|s| s.parse::<u32>().ok())
                                .filter(|&v| v > 0)
                                .unwrap_or(3);
                            let normal = child_text(vc.inner, "normalNotes")
                                .and_then(|s| s.parse::<u32>().ok())
                                .filter(|&v| v > 0)
                                .unwrap_or(2);
                            tuplet_stack.push((actual, normal));
                        }
                        "endTuplet" => {
                            tuplet_stack.pop();
                        }
                        "Fermata" => {
                            pending_fermata = true;
                        }
                        "Rest" => {
                            let dtype = child_text(vc.inner, "durationType")
                                .unwrap_or_else(|| "quarter".to_string());
                            let dots: u32 = child_text(vc.inner, "dots")
                                .and_then(|s| s.parse().ok())
                                .unwrap_or(0);
                            let written = if dtype == "measure" {
                                child_text(vc.inner, "duration")
                                    .and_then(|s| parse_fraction_beats(&s))
                                    .unwrap_or(barlen)
                            } else {
                                let base = duration_type_beats(&dtype).unwrap_or(1.0);
                                base * (2.0 - 2f64.powi(-(dots as i32)))
                            };
                            let scale = tuplet_stack
                                .iter()
                                .fold(1.0, |acc, &(a, n)| acc * n as f64 / a as f64);
                            let cur_tup = tuplet_stack.last().copied();
                            let dur = written * scale;
                            let mut arts = Vec::new();
                            if pending_fermata {
                                arts.push("fermata".to_string());
                                pending_fermata = false;
                            }
                            let xstaff = child_text(vc.inner, "staffMove")
                                .and_then(|s| s.parse::<i8>().ok())
                                .filter(|&d| d != 0);
                            notes.push(NoteInput {
                                pitch: None,
                                rest: true,
                                onset: bar_start + cursor,
                                dur,
                                voice: voice_num,
                                lyric: None,
                                grace: false,
                                grace_group: None,
                                slur_start: false,
                                slur_stop: false,
                                articulations: arts,
                                tie_start: false,
                                tie_stop: false,
                                tuplet: cur_tup,
                                cue: false,
                                xstaff,
                            });
                            cursor += dur;
                        }
                        "Chord" => {
                            let chord_children = direct_children(vc.inner);
                            let dtype = child_text(vc.inner, "durationType")
                                .unwrap_or_else(|| "quarter".to_string());
                            let dots: u32 = child_text(vc.inner, "dots")
                                .and_then(|s| s.parse().ok())
                                .unwrap_or(0);
                            let base = duration_type_beats(&dtype).unwrap_or(1.0);
                            let written = base * (2.0 - 2f64.powi(-(dots as i32)));
                            let is_grace = chord_children.iter().any(|c| {
                                matches!(
                                    c.name,
                                    "appoggiatura"
                                        | "acciaccatura"
                                        | "grace4"
                                        | "grace8"
                                        | "grace16"
                                        | "grace32"
                                        | "grace8after"
                                        | "grace16after"
                                        | "grace32after"
                                )
                            });
                            if is_grace {
                                grace_seq = grace_seq.saturating_add(1);
                            }
                            let chord_cue = child_text(vc.inner, "small").as_deref() == Some("1");
                            let xstaff = child_text(vc.inner, "staffMove")
                                .and_then(|s| s.parse::<i8>().ok())
                                .filter(|&d| d != 0);
                            let lyric = chord_children
                                .iter()
                                .find(|c| c.name == "Lyrics")
                                .and_then(|l| child_text(l.inner, "text"))
                                .map(|t| strip_inline_tags(&t))
                                .filter(|s| !s.is_empty());

                            let mut slur_start = false;
                            let mut slur_stop = false;
                            let mut arts: Vec<String> = Vec::new();
                            if pending_fermata {
                                arts.push("fermata".to_string());
                                pending_fermata = false;
                            }
                            for cc in &chord_children {
                                if cc.name == "Spanner"
                                    && attr_value(cc.attrs, "type") == Some("Slur")
                                {
                                    if has_child(cc.inner, "next") {
                                        slur_start = true;
                                    }
                                    if has_child(cc.inner, "prev") {
                                        slur_stop = true;
                                    }
                                } else if cc.name == "Articulation" {
                                    if let Some(sub) = child_text(cc.inner, "subtype") {
                                        if let Some(a) = parse_mscx_articulation(&sub) {
                                            if !arts.iter().any(|x| x == a) {
                                                arts.push(a.to_string());
                                            }
                                        }
                                    }
                                }
                            }

                            let scale = tuplet_stack
                                .iter()
                                .fold(1.0, |acc, &(a, n)| acc * n as f64 / a as f64);
                            let cur_tup = if is_grace {
                                None
                            } else {
                                tuplet_stack.last().copied()
                            };
                            let performed = if is_grace { written } else { written * scale };
                            let onset = bar_start + cursor;

                            let mut first_in_chord = true;
                            for cc in &chord_children {
                                if cc.name != "Note" {
                                    continue;
                                }
                                let Some(midi) = child_text(cc.inner, "pitch")
                                    .and_then(|s| s.parse::<i32>().ok())
                                else {
                                    continue;
                                };
                                let tpc_opt = child_text(cc.inner, "tpc")
                                    .and_then(|s| s.parse::<i32>().ok());
                                let pitch_str = mscx_pitch_string(midi, tpc_opt, cur_key);
                                let note_cue = chord_cue
                                    || child_text(cc.inner, "small").as_deref() == Some("1");
                                let mut tie_start = false;
                                let mut tie_stop = false;
                                for nc in direct_children(cc.inner) {
                                    if nc.name == "Spanner"
                                        && attr_value(nc.attrs, "type") == Some("Tie")
                                    {
                                        if has_child(nc.inner, "next") {
                                            tie_start = true;
                                        }
                                        if has_child(nc.inner, "prev") {
                                            tie_stop = true;
                                        }
                                    }
                                }
                                notes.push(NoteInput {
                                    pitch: Some(pitch_str),
                                    rest: false,
                                    onset,
                                    dur: performed,
                                    voice: voice_num,
                                    lyric: if first_in_chord { lyric.clone() } else { None },
                                    grace: is_grace,
                                    grace_group: if is_grace { Some(grace_seq) } else { None },
                                    slur_start: if first_in_chord { slur_start } else { false },
                                    slur_stop: if first_in_chord { slur_stop } else { false },
                                    articulations: if first_in_chord {
                                        arts.clone()
                                    } else {
                                        Vec::new()
                                    },
                                    tie_start,
                                    tie_stop,
                                    tuplet: cur_tup,
                                    cue: note_cue,
                                    xstaff,
                                });
                                first_in_chord = false;
                            }
                            if !is_grace {
                                cursor += performed;
                            }
                        }
                        _ => {}
                    }
                }
            }
            bar_start += barlen;
        }

        p_measure_attrs.sort_by_key(|a| a.measure);
        let (clef_sign, clef_line, clef_octave_change) = match part_clef {
            Some((s, l, o)) => (Some(s), Some(l), o),
            None => (None, None, None),
        };
        parts.push(PartInput {
            name: part_name,
            notes,
            clef_sign,
            clef_line,
            clef_octave_change,
            key_fifths: part_key,
            measure_attrs: p_measure_attrs,
        });
    }

    Ok(ScoreInput {
        title,
        composer,
        divisions,
        time: Some(score_time.unwrap_or(TimeSig {
            beats: 4,
            beat_type: 4,
        })),
        key_fifths: score_key_fifths.unwrap_or(0),
        parts,
        notes: Vec::new(),
        part_name: None,
        measure_attrs: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{parse_score, to_mscx};

    #[test]
    fn tpc_to_step_alter_inverts_tonal_pitch_class() {
        for step in ['C', 'D', 'E', 'F', 'G', 'A', 'B'] {
            for alter in -2..=2 {
                let tpc = crate::pitch::tonal_pitch_class(step, alter);
                assert_eq!(tpc_to_step_alter(tpc), (step, alter));
            }
        }
    }

    #[test]
    fn roundtrip_to_mscx_into_import_mscx_preserves_notes_pitches_durations_and_headers() {
        let mut n1 = NoteInput::note("C#4", 0.0, 1.0);
        n1.lyric = Some("La".to_string());
        n1.articulations.push("stacc".to_string());
        n1.slur_start = true;

        let mut n2 = NoteInput::note("Db4", 1.0, 1.0);
        n2.slur_stop = true;
        n2.tie_start = true;

        let mut n3 = NoteInput::note("Db4", 2.0, 2.0);
        n3.tie_stop = true;

        let mut n4 = NoteInput::note("F#5", 4.0, 1.5);
        n4.cue = true;
        n4.xstaff = Some(-1);

        let n5 = NoteInput::note("A4", 5.5, 0.5);
        let r1 = NoteInput::rest(6.0, 2.0);

        let mut m1 = MeasureAttr {
            measure: 1,
            volta: Some("1".to_string()),
            ..Default::default()
        };
        m1.push_dynamic("mf");
        m1.timed_events.push(TimedEvent {
            onset_beats: 0.0,
            kind: "ottava".to_string(),
            value: "8va".to_string(),
        });

        let m2 = MeasureAttr {
            measure: 2,
            barline_right: Some("light-heavy".to_string()),
            timed_events: vec![TimedEvent {
                onset_beats: 0.0,
                kind: "ottava".to_string(),
                value: "stop".to_string(),
            }],
            ..Default::default()
        };

        let input = ScoreInput {
            title: Some("Roundtrip Etude".to_string()),
            composer: Some("Test Composer".to_string()),
            divisions: SCORE_DIVISIONS,
            time: Some(TimeSig {
                beats: 4,
                beat_type: 4,
            }),
            key_fifths: 2,
            parts: vec![PartInput {
                name: "Violin".to_string(),
                notes: vec![n1, n2, n3, n4, n5, r1],
                clef_sign: Some('G'),
                clef_line: Some(2),
                clef_octave_change: None,
                key_fifths: Some(2),
                measure_attrs: Vec::new(),
            }],
            notes: Vec::new(),
            part_name: None,
            measure_attrs: vec![m1, m2],
        };

        let (score, mut losses) = parse_score(&input).unwrap();
        let mscx = to_mscx(&score, &mut losses);
        let imported = import_mscx(&mscx).expect("import_mscx succeeds");

        assert_eq!(imported.title.as_deref(), Some("Roundtrip Etude"));
        assert_eq!(imported.composer.as_deref(), Some("Test Composer"));
        assert_eq!(
            imported.time,
            Some(TimeSig {
                beats: 4,
                beat_type: 4
            })
        );
        assert_eq!(imported.key_fifths, 2);
        assert_eq!(imported.parts.len(), 1);

        let part = &imported.parts[0];
        assert_eq!(part.name, "Violin");
        assert_eq!(part.clef_sign, Some('G'));
        assert_eq!(part.clef_line, Some(2));
        assert_eq!(part.key_fifths, Some(2));

        let pitched: Vec<_> = part.notes.iter().filter(|n| !n.rest).collect();
        assert_eq!(pitched.len(), 5);
        assert_eq!(pitched[0].pitch.as_deref(), Some("C#4"));
        assert!((pitched[0].dur - 1.0).abs() < 1e-6);
        assert_eq!(pitched[0].lyric.as_deref(), Some("La"));
        assert!(pitched[0].slur_start);
        assert!(pitched[0].articulations.contains(&"stacc".to_string()));

        assert_eq!(pitched[1].pitch.as_deref(), Some("Db4"));
        assert!(pitched[1].tie_start);
        assert!(pitched[1].slur_stop);

        assert_eq!(pitched[2].pitch.as_deref(), Some("Db4"));
        assert!(pitched[2].tie_stop);
        assert!((pitched[2].dur - 2.0).abs() < 1e-6);

        assert_eq!(pitched[3].pitch.as_deref(), Some("F#5"));
        assert!((pitched[3].dur - 1.5).abs() < 1e-6);
        assert!(pitched[3].cue);
        assert_eq!(pitched[3].xstaff, Some(-1));

        assert_eq!(pitched[4].pitch.as_deref(), Some("A4"));
        assert!((pitched[4].dur - 0.5).abs() < 1e-6);

        // Verify measure attrs: dynamic, volta, ottava, final barline.
        let im1 = part.measure_attrs.iter().find(|a| a.measure == 1).unwrap();
        assert_eq!(im1.dynamic.as_deref(), Some("mf"));
        assert_eq!(im1.volta.as_deref(), Some("1"));
        assert!(im1
            .timed_events
            .iter()
            .any(|te| te.kind == "ottava" && te.value == "8va"));

        let im2 = part.measure_attrs.iter().find(|a| a.measure == 2).unwrap();
        assert_eq!(im2.barline_right.as_deref(), Some("light-heavy"));
    }

    #[test]
    fn import_mscx_extracts_harmony_tempo_and_ms3_keysig() {
        let ms3_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<museScore version="3.02">
  <Score>
    <Division>480</Division>
    <metaTag name="workTitle">Lead Sheet</metaTag>
    <Part>
      <Staff id="1"/>
      <trackName>Piano</trackName>
    </Part>
    <Staff id="1">
      <Measure>
        <voice>
          <Clef><clefType>G</clefType></Clef>
          <KeySig><accidental>-1</accidental></KeySig>
          <TimeSig><sigN>3</sigN><sigD>4</sigD></TimeSig>
          <Tempo><tempo>2.0</tempo><text>Allegro</text></Tempo>
          <Harmony><root>13</root><name>maj7</name><base>17</base></Harmony>
          <Dynamic><subtype>f</subtype></Dynamic>
          <Chord>
            <durationType>half</durationType>
            <dots>1</dots>
            <Lyrics><text>Hi</text></Lyrics>
            <Note><pitch>65</pitch><tpc>13</tpc></Note>
            <Note><pitch>69</pitch><tpc>17</tpc></Note>
          </Chord>
        </voice>
      </Measure>
    </Staff>
  </Score>
</museScore>"#;

        let imported = import_mscx(ms3_xml).expect("MS3 import succeeds");
        assert_eq!(imported.title.as_deref(), Some("Lead Sheet"));
        assert_eq!(
            imported.time,
            Some(TimeSig {
                beats: 3,
                beat_type: 4
            })
        );
        assert_eq!(imported.key_fifths, -1);
        assert_eq!(imported.parts.len(), 1);
        let part = &imported.parts[0];
        assert_eq!(part.name, "Piano");
        assert_eq!(part.notes.len(), 2);
        assert_eq!(part.notes[0].pitch.as_deref(), Some("F4"));
        assert_eq!(part.notes[1].pitch.as_deref(), Some("A4"));
        assert!((part.notes[0].dur - 3.0).abs() < 1e-6);
        assert_eq!(part.notes[0].lyric.as_deref(), Some("Hi"));

        let ma = part.measure_attrs.iter().find(|a| a.measure == 1).unwrap();
        assert_eq!(ma.dynamic.as_deref(), Some("f"));
        assert!(ma
            .timed_events
            .iter()
            .any(|te| te.kind == "harmony" && te.value == "Fmaj7/A"));
        assert!(ma
            .timed_events
            .iter()
            .any(|te| te.kind == "tempo" && te.value == "Allegro (120)"));
    }

    #[test]
    fn import_mscx_rejects_non_mscx() {
        assert!(import_mscx("<score-partwise/>").is_err());
    }
}
