//! Example 1: Compose a two-voice miniature prelude in pure Rust using
//! [`ScoreBuilder`] and [`PartBuilder::chord`], serialize it to MusicXML 3.1,
//! MuseScore 4 `.mscx`, and `.mscz` in memory, validate all three stages,
//! and inspect the [`LossTable`].
//!
//! Run with:
//! ```bash
//! cargo run --example 01_compose_two_voice
//! ```

use mscz::{
    has_errors, pack_mscz, parse_score, to_mscx, to_musicxml, validate_mscz, validate_musicxml,
    validate_notes, MeasureAttr, ScoreBuilder,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input = ScoreBuilder::new("Miniature Two-Voice Prelude")
        .composer("mscz-rs Example")
        .time(4, 4)
        .key_fifths(0)
        .measure_attr(
            1,
            MeasureAttr {
                dynamic: Some("mp".into()),
                hairpin: Some("cresc".into()),
                ..Default::default()
            },
        )
        .measure_attr(
            2,
            MeasureAttr {
                dynamic: Some("mf".into()),
                hairpin: Some("stop".into()),
                barline_right: Some("light-heavy".into()),
                ..Default::default()
            },
        )
        .part("Piano", |p| {
            // Voice 1 (melody): slurred opening motif with articulations, a note
            // crossing the 4/4 barline (beats 3.0..5.0; split & tied automatically),
            // and an intentional microtone ("A4+30c") recorded in the LossTable.
            p.note("E5", 0.0, 1.0)
                .slur_start()
                .tenuto()
                .note("D5", 1.0, 1.0)
                .staccato()
                .note("C5", 2.0, 1.0)
                .slur_stop()
                .note("B4", 3.0, 2.0)
                .accent()
                .note("A4+30c", 5.0, 1.0)
                .note("G4", 6.0, 2.0)
                .fermata()
                // Voice 2 (accompaniment): simultaneous chords in one line each.
                .voice(2)
                .chord(&["C4", "G4"], 0.0, 2.0)
                .chord(&["B3", "G4"], 2.0, 2.0)
                .chord(&["D4", "F4"], 4.0, 2.0)
                .chord(&["C4", "E4", "G4"], 6.0, 2.0)
        })
        .build_input();

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
