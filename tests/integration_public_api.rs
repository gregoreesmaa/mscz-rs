//! End-to-end integration tests exercising the public `mscz` library API.

use std::fs;
use std::path::PathBuf;

#[cfg(feature = "bekern")]
use mscz::{bekern, decode, ser_for_inputs};
use mscz::{
    example_input, has_errors, is_empty_score_input, musicxml_import, pack_mscz, parse_score,
    score_pitched_count, to_mscx, to_musicxml, validate_mscz, validate_musicxml, validate_notes,
    NoteInput, PartInput, ScoreInput, TimeSig, SCORE_DIVISIONS,
};

const CC0_FIXTURES: &[&str] = &["ds0001.musicxml", "ds0037.musicxml", "ds0409.musicxml"];

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

#[test]
fn roundtrip_all_cc0_fixtures_through_mscz() {
    for &fixture in CC0_FIXTURES {
        let xml = fs::read_to_string(fixture_path(fixture))
            .unwrap_or_else(|e| panic!("failed to read {fixture}: {e}"));

        let input = musicxml_import::score_input_from_musicxml(&xml)
            .unwrap_or_else(|e| panic!("{fixture}: score_input_from_musicxml failed: {e}"));
        assert!(
            !is_empty_score_input(&input),
            "{fixture}: imported ScoreInput should not be empty"
        );

        let note_issues = validate_notes(&input);
        assert!(
            !has_errors(&note_issues),
            "{fixture}: validate_notes errors: {note_issues:?}"
        );

        let (score, mut losses) =
            parse_score(&input).unwrap_or_else(|e| panic!("{fixture}: parse_score failed: {e}"));
        assert!(!score.parts.is_empty(), "{fixture}: expected >= 1 part");
        assert!(
            score.measure_count() > 0,
            "{fixture}: expected >= 1 measure"
        );
        assert!(
            score_pitched_count(&score) > 0,
            "{fixture}: expected pitched notes"
        );

        let rendered_xml = to_musicxml(&score, &mut losses);
        let xml_issues = validate_musicxml(&rendered_xml);
        assert!(
            !has_errors(&xml_issues),
            "{fixture}: validate_musicxml errors: {xml_issues:?}"
        );

        let mscx = to_mscx(&score, &mut losses);
        assert!(
            mscx.contains("<museScore"),
            "{fixture}: .mscx missing <museScore> root"
        );

        let mscz_bytes =
            pack_mscz(&score, &mscx).unwrap_or_else(|e| panic!("{fixture}: pack_mscz failed: {e}"));
        assert!(!mscz_bytes.is_empty(), "{fixture}: .mscz bytes empty");

        let mscz_issues = validate_mscz(&mscz_bytes);
        assert!(
            !has_errors(&mscz_issues),
            "{fixture}: validate_mscz errors: {mscz_issues:?}"
        );
    }
}

#[test]
fn mscz_output_is_byte_identical_across_repeated_runs() {
    let build_mscz = |input: &ScoreInput| -> Vec<u8> {
        let (score, mut losses) = parse_score(input).expect("parse_score");
        let mscx = to_mscx(&score, &mut losses);
        pack_mscz(&score, &mscx).expect("pack_mscz")
    };

    // Built-in example input
    let ex = example_input();
    let ex_a = build_mscz(&ex);
    let ex_b = build_mscz(&ex);
    assert_eq!(
        ex_a, ex_b,
        "example_input .mscz output must be deterministic"
    );

    // All three CC0 fixtures
    for &fixture in CC0_FIXTURES {
        let xml = fs::read_to_string(fixture_path(fixture)).expect("read fixture");
        let input = musicxml_import::score_input_from_musicxml(&xml).expect("import musicxml");
        let run1 = build_mscz(&input);
        let run2 = build_mscz(&input);
        assert_eq!(
            run1, run2,
            "{fixture}: .mscz output must be byte-identical across runs"
        );
    }
}

#[cfg(feature = "bekern")]
#[test]
fn bekern_roundtrip_and_ser_on_cc0_fixture() {
    let xml = fs::read_to_string(fixture_path("ds0001.musicxml")).expect("read ds0001");
    let events = musicxml_import::bekern_events(&xml).expect("bekern_events");
    let headers = musicxml_import::staff_headers(&xml).expect("staff_headers");
    let (tokens, _skipped) = bekern::to_bekern(&events, &headers).expect("to_bekern");
    assert!(!tokens.is_empty(), "expected non-empty bekern token stream");

    let token_str = tokens.join(" ");
    let decoded = decode::decode_bekern_unmerged(&token_str).expect("decode_bekern_unmerged");
    let (score, mut losses) = parse_score(&decoded).expect("parse_score");
    let mscx = to_mscx(&score, &mut losses);
    let mscz_bytes = pack_mscz(&score, &mscx).expect("pack_mscz");
    let issues = validate_mscz(&mscz_bytes);
    assert!(
        !has_errors(&issues),
        "bekern roundtrip .mscz errors: {issues:?}"
    );

    let reference = musicxml_import::score_input_from_musicxml(&xml).expect("reference import");
    let (report, ref_syms, hyp_syms) =
        ser_for_inputs(&reference, &decoded).expect("ser_for_inputs");
    assert!(!ref_syms.is_empty());
    assert!(!hyp_syms.is_empty());
    assert!(report.ser.is_finite());

    let (self_report, _, _) = ser_for_inputs(&decoded, &decoded).expect("self ser_for_inputs");
    assert_eq!(self_report.distance, 0);
    assert_eq!(self_report.ser, 0.0);
}

#[test]
fn microtone_records_loss_table_entry() {
    let input = ScoreInput {
        title: Some("Microtone Test".to_string()),
        composer: None,
        divisions: SCORE_DIVISIONS,
        time: Some(TimeSig {
            beats: 4,
            beat_type: 4,
        }),
        key_fifths: 0,
        parts: vec![PartInput {
            name: "Violin".to_string(),
            notes: vec![NoteInput {
                pitch: Some("A4+30c".to_string()),
                rest: false,
                onset: 0.0,
                dur: 1.0,
                voice: 1,
                lyric: None,
                grace: false,
                slur_start: false,
                slur_stop: false,
                articulations: Vec::new(),
                tie_start: false,
                tie_stop: false,
            }],
            ..Default::default()
        }],
        notes: Vec::new(),
        part_name: None,
        measure_attrs: Vec::new(),
    };

    let (_score, losses) = parse_score(&input).expect("parse_score");
    assert!(!losses.is_lossless());
    assert_eq!(losses.loss_count(), 1);
    assert!(
        losses
            .entries
            .iter()
            .any(|e| e.kind == "pitch-microtone-rounded"),
        "expected pitch-microtone-rounded loss entry, got: {:?}",
        losses.entries
    );
}

#[test]
fn invalid_inputs_return_errors_and_diagnostics() {
    // 1. Empty score
    let empty = ScoreInput {
        title: None,
        composer: None,
        divisions: SCORE_DIVISIONS,
        time: None,
        key_fifths: 0,
        parts: Vec::new(),
        notes: Vec::new(),
        part_name: None,
        measure_attrs: Vec::new(),
    };
    assert!(is_empty_score_input(&empty));
    assert!(parse_score(&empty).is_err());
    assert!(has_errors(&validate_notes(&empty)));

    // 2. Zero divisions
    let mut bad_div = example_input();
    bad_div.divisions = 0;
    assert!(parse_score(&bad_div).is_err());
    assert!(has_errors(&validate_notes(&bad_div)));

    // 3. Invalid pitch, negative onset, non-positive duration
    let bad_note = ScoreInput {
        title: Some("Invalid".to_string()),
        composer: None,
        divisions: SCORE_DIVISIONS,
        time: None,
        key_fifths: 0,
        parts: Vec::new(),
        notes: vec![NoteInput {
            pitch: Some("Z99".to_string()),
            rest: false,
            onset: -0.5,
            dur: 0.0,
            voice: 1,
            lyric: None,
            grace: false,
            slur_start: false,
            slur_stop: false,
            articulations: Vec::new(),
            tie_start: false,
            tie_stop: false,
        }],
        part_name: Some("Part 1".to_string()),
        measure_attrs: Vec::new(),
    };
    assert!(parse_score(&bad_note).is_err());
    assert!(has_errors(&validate_notes(&bad_note)));

    // 4. Malformed MusicXML
    let bad_xml = "<not-musicxml><part/></not-musicxml>";
    assert!(musicxml_import::score_input_from_musicxml(bad_xml).is_err());
    assert!(has_errors(&validate_musicxml(bad_xml)));

    // 5. Corrupted .mscz bytes
    assert!(has_errors(&validate_mscz(b"not a zip archive")));
    assert!(has_errors(&validate_mscz(&[])));
}
