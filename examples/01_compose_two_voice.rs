//! Example 1: Compose a two-voice miniature prelude in pure Rust, serialize it
//! to MusicXML 3.1, MuseScore 4 `.mscx`, and `.mscz` in memory, validate all
//! three stages, and inspect the [`LossTable`].
//!
//! Run with:
//! ```bash
//! cargo run --example 01_compose_two_voice
//! ```

use mscz::{
    has_errors, pack_mscz, parse_score, to_mscx, to_musicxml, validate_mscz, validate_musicxml,
    validate_notes, MeasureAttr, NoteInput, PartInput, ScoreInput, TimeSig, SCORE_DIVISIONS,
};

fn note(pitch: &str, onset: f64, dur: f64, voice: u32) -> NoteInput {
    NoteInput {
        pitch: Some(pitch.to_string()),
        rest: false,
        onset,
        dur,
        voice,
        lyric: None,
        grace: false,
        slur_start: false,
        slur_stop: false,
        articulations: Vec::new(),
        tie_start: false,
        tie_stop: false,
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Voice 1 (melody): slurred opening motif with articulations, a note tied
    // across the barline (beat 3.0 -> 5.0), and an intentional microtone
    // ("A4+30c") that triggers a recorded approximation in the LossTable.
    let mut m1_v1_a = note("E5", 0.0, 1.0, 1);
    m1_v1_a.slur_start = true;
    m1_v1_a.articulations.push("tenuto".to_string());

    let mut m1_v1_b = note("D5", 1.0, 1.0, 1);
    m1_v1_b.articulations.push("stacc".to_string());

    let mut m1_v1_c = note("C5", 2.0, 1.0, 1);
    m1_v1_c.slur_stop = true;

    // Crosses the 4/4 barline (beats 3.0..5.0); serializer splits and ties it.
    let mut m1_v1_tie = note("B4", 3.0, 2.0, 1);
    m1_v1_tie.articulations.push("accent".to_string());

    // Intentional microtone (+30 cents): rounded to nearest semitone and logged.
    let m2_v1_micro = note("A4+30c", 5.0, 1.0, 1);

    let mut m2_v1_end = note("G4", 6.0, 2.0, 1);
    m2_v1_end.articulations.push("fermata".to_string());

    // Voice 2 (accompaniment): simultaneous same-voice notes form chords.
    let v2_notes = vec![
        // Measure 1, beats 0..2: C major dyad
        note("C4", 0.0, 2.0, 2),
        note("G4", 0.0, 2.0, 2),
        // Measure 1, beats 2..4: G major dyad
        note("B3", 2.0, 2.0, 2),
        note("G4", 2.0, 2.0, 2),
        // Measure 2, beats 4..6: D minor dyad
        note("D4", 4.0, 2.0, 2),
        note("F4", 4.0, 2.0, 2),
        // Measure 2, beats 6..8: C major triad
        note("C4", 6.0, 2.0, 2),
        note("E4", 6.0, 2.0, 2),
        note("G4", 6.0, 2.0, 2),
    ];

    let mut notes = vec![m1_v1_a, m1_v1_b, m1_v1_c, m1_v1_tie, m2_v1_micro, m2_v1_end];
    notes.extend(v2_notes);

    let input = ScoreInput {
        title: Some("Miniature Two-Voice Prelude".to_string()),
        composer: Some("mscz-rs Example".to_string()),
        divisions: SCORE_DIVISIONS,
        time: Some(TimeSig {
            beats: 4,
            beat_type: 4,
        }),
        key_fifths: 0,
        parts: vec![PartInput {
            name: "Piano".to_string(),
            notes,
        }],
        notes: Vec::new(),
        part_name: None,
        measure_attrs: vec![
            MeasureAttr {
                measure: 1,
                dynamic: Some("mp".to_string()),
                hairpin: Some("cresc".to_string()),
                ..MeasureAttr::default()
            },
            MeasureAttr {
                measure: 2,
                dynamic: Some("mf".to_string()),
                hairpin: Some("stop".to_string()),
                barline_right: Some("light-heavy".to_string()),
                ..MeasureAttr::default()
            },
        ],
    };

    // 1. Validate raw ScoreInput
    let note_issues = validate_notes(&input);
    assert!(
        !has_errors(&note_issues),
        "unexpected input errors: {note_issues:?}"
    );

    // 2. Parse into tick-based Score + LossTable
    let (score, mut losses) = parse_score(&input)?;

    // 3. Serialize to MusicXML 3.1, MuseScore 4 .mscx, and .mscz in memory
    let musicxml = to_musicxml(&score, &mut losses);
    let mscx = to_mscx(&score, &mut losses);
    let mscz_bytes = pack_mscz(&score, &mscx)?;

    // 4. Validate MusicXML and .mscz
    let xml_issues = validate_musicxml(&musicxml);
    let mscz_issues = validate_mscz(&mscz_bytes);
    assert!(
        !has_errors(&xml_issues),
        "unexpected MusicXML errors: {xml_issues:?}"
    );
    assert!(
        !has_errors(&mscz_issues),
        "unexpected .mscz errors: {mscz_issues:?}"
    );

    println!("Title:    {}", score.title);
    println!(
        "Layout:   {} part(s), {} measure(s)",
        score.parts.len(),
        score.measure_count()
    );
    println!(
        "MusicXML: {} bytes ({} issue(s))",
        musicxml.len(),
        xml_issues.len()
    );
    println!("MSCX:     {} bytes", mscx.len());
    println!(
        "MSCZ:     {} bytes ({} issue(s))",
        mscz_bytes.len(),
        mscz_issues.len()
    );

    println!(
        "\nLossTable: {} total entry/entries ({} loss, lossless={})",
        losses.entries.len(),
        losses.loss_count(),
        losses.is_lossless()
    );
    for entry in &losses.entries {
        println!(
            "  [{}] {} — {} (part={:?}, note_index={:?})",
            entry.severity, entry.kind, entry.message, entry.part, entry.note_index
        );
    }

    Ok(())
}
