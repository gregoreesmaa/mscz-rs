//! Example 4: End-to-end `bekern+` ML token pipeline roundtrip.
//!
//! 1. Reads the CC0 OpenScore fixture `tests/fixtures/ds0001.musicxml`,
//!    tokenizes it via `musicxml_import::bekern_events` -> `bekern::to_bekern`,
//!    decodes the token stream with `decode::decode_bekern_unmerged`, and packs
//!    a validated native MuseScore 4 `.mscz` archive.
//! 2. Round-trips a single-part melody through `ScoreBuilder` -> `to_musicxml`
//!    -> `bekern_events` -> `to_bekern` -> `decode_bekern` and verifies `0.00%`
//!    Symbol Error Rate (`ser_for_inputs`).
//!
//! Run with:
//! ```bash
//! cargo run --example 04_bekern_ml_roundtrip
//! ```

use std::fs;
use std::path::PathBuf;

use mscz::{
    bekern, decode, has_errors, musicxml_import, pack_mscz, parse_score, ser_for_inputs, to_mscx,
    to_musicxml, validate_mscz, ScoreBuilder,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("ds0001.musicxml");
    let xml = fs::read_to_string(&fixture)?;

    // 1. Extract bekern events + staff headers and encode to bekern+ tokens
    let events = musicxml_import::bekern_events(&xml)?;
    let headers = musicxml_import::staff_headers(&xml)?;
    let (tokens, skipped) = bekern::to_bekern(&events, &headers)?;
    let token_stream = tokens.join(" ");

    println!("Fixture:  {}", fixture.display());
    println!("Events:   {} (skipped {skipped})", events.len());
    println!("Tokens:   {}", tokens.len());
    let preview: Vec<&str> = tokens.iter().take(18).map(String::as_str).collect();
    println!("Preview:  {} ...", preview.join(" "));

    // 2. Decode bekern+ tokens (link-faithful, unmerged ties) -> Score -> .mscz
    let decoded_unmerged = decode::decode_bekern_unmerged(&token_stream)?;
    let (score, mut losses) = parse_score(&decoded_unmerged)?;
    let mscx = to_mscx(&score, &mut losses);
    let mscz_bytes = pack_mscz(&score, &mscx)?;

    let issues = validate_mscz(&mscz_bytes);
    assert!(
        !has_errors(&issues),
        "roundtripped .mscz failed validation: {issues:?}"
    );

    println!(
        "Decoded:  {} part(s), {} measure(s), {} bytes .mscz (valid)",
        score.parts.len(),
        score.measure_count(),
        mscz_bytes.len()
    );

    // 3. Exact single-part melody round-trip: ScoreInput -> MusicXML -> bekern+ -> ScoreInput -> SER
    let melody_input = ScoreBuilder::new("Melody Roundtrip")
        .time(4, 4)
        .key_fifths(1)
        .part("Violin", |p| {
            p.note("G4", 0.0, 1.0)
                .note("A4", 1.0, 0.5)
                .note("B4", 1.5, 0.5)
                .note("C5", 2.0, 1.0)
                .rest(3.0, 1.0)
                .note("D5", 4.0, 1.5)
                .note("F#4", 5.5, 0.5)
                .note("G4", 6.0, 2.0)
        })
        .build_input();

    let (melody_score, mut melody_losses) = parse_score(&melody_input)?;
    let melody_xml = to_musicxml(&melody_score, &mut melody_losses);
    let melody_events = musicxml_import::bekern_events(&melody_xml)?;
    let melody_headers = musicxml_import::staff_headers(&melody_xml)?;
    let (melody_tokens, _) = bekern::to_bekern(&melody_events, &melody_headers)?;
    let melody_decoded = decode::decode_bekern(&melody_tokens.join(" "))?;

    let (ser_report, _, _) = ser_for_inputs(&melody_input, &melody_decoded)?;
    assert_eq!(ser_report.distance, 0);
    println!(
        "Melody bekern+ tokens ({}): {}",
        melody_tokens.len(),
        melody_tokens.join(" ")
    );
    println!(
        "SER (single-part round-trip): {:.2}% (dist={}, ref={}, hyp={})",
        ser_report.ser * 100.0,
        ser_report.distance,
        ser_report.ref_len,
        ser_report.hyp_len
    );

    Ok(())
}
