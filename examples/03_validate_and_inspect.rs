//! Example 3: CI-style validation and structured diagnostic inspection across
//! `validate_notes`, `validate_musicxml`, and `validate_mscz`.
//!
//! Demonstrates both a clean, valid `.mscz` archive and deliberately corrupted
//! inputs so you can see the structured [`Issue`] codes and [`has_errors`]
//! gating in action.
//!
//! Run with:
//! ```bash
//! cargo run --example 03_validate_and_inspect
//! ```

use std::io::{Cursor, Write};

use mscz::{
    example_input, has_errors, pack_mscz, parse_score, to_mscx, to_musicxml, validate_mscz,
    validate_musicxml, validate_notes, Issue, NoteInput, ScoreInput,
};

fn print_report(label: &str, issues: &[Issue]) {
    let status = if has_errors(issues) { "FAIL" } else { "PASS" };
    println!("=== {label} [{status}] ({} issue(s)) ===", issues.len());
    for issue in issues {
        println!("  [{}] {}: {}", issue.severity, issue.code, issue.message);
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Valid end-to-end score
    let valid_input = example_input();
    let note_issues = validate_notes(&valid_input);
    print_report("Valid ScoreInput", &note_issues);
    assert!(!has_errors(&note_issues));

    let (score, mut losses) = parse_score(&valid_input)?;
    let valid_xml = to_musicxml(&score, &mut losses);
    let xml_issues = validate_musicxml(&valid_xml);
    print_report("Valid MusicXML 3.1", &xml_issues);
    assert!(!has_errors(&xml_issues));

    let valid_mscx = to_mscx(&score, &mut losses);
    let valid_mscz = pack_mscz(&score, &valid_mscx)?;
    let mscz_issues = validate_mscz(&valid_mscz);
    print_report("Valid .mscz archive", &mscz_issues);
    assert!(!has_errors(&mscz_issues));

    // 2. Deliberately invalid ScoreInput (zero divisions, negative onset, bad pitch)
    let bad_input = ScoreInput {
        title: Some("Broken Input".to_string()),
        composer: None,
        divisions: 0,
        time: None,
        key_fifths: 0,
        parts: Vec::new(),
        notes: vec![NoteInput {
            pitch: Some("Z99".to_string()),
            onset: -1.0,
            dur: 0.0,
            voice: 0,
            ..Default::default()
        }],
        part_name: Some("Broken".to_string()),
        measure_attrs: Vec::new(),
    };
    let bad_note_issues = validate_notes(&bad_input);
    print_report("Invalid ScoreInput", &bad_note_issues);
    assert!(has_errors(&bad_note_issues));

    // 3. Deliberately malformed MusicXML
    let bad_xml = "<not-musicxml><broken/></not-musicxml>";
    let bad_xml_issues = validate_musicxml(bad_xml);
    print_report("Corrupted MusicXML", &bad_xml_issues);
    assert!(has_errors(&bad_xml_issues));

    // 4a. Deliberately corrupted .mscz (non-zip bytes)
    let garbage_zip_issues = validate_mscz(b"PK-not-a-real-zip-archive");
    print_report("Corrupted .mscz (garbage bytes)", &garbage_zip_issues);
    assert!(has_errors(&garbage_zip_issues));

    // 4b. Valid zip archive missing required MuseScore container/score entries
    let mut buf = Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default();
        zip.start_file("README.txt", opts)?;
        zip.write_all(b"not a MuseScore archive")?;
        zip.finish()?;
    }
    let incomplete_zip_issues = validate_mscz(&buf.into_inner());
    print_report(
        "Corrupted .mscz (missing container)",
        &incomplete_zip_issues,
    );
    assert!(has_errors(&incomplete_zip_issues));

    Ok(())
}
