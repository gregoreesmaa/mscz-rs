//! Comprehensive MuseScore 4 & MusicXML 3.1 parity integration test suite.
//!
//! Covers single-staff, 2-staff piano grand staff, 3-staff voice+piano,
//! 4-staff string quartet, multi-voice counterpoint, repeats, tuplets,
//! double accidentals, dynamics/hairpins, key/time/clef changes, and
//! multi-page `stitch_pages_bekern` — verifying exact note counts, MIDI
//! pitches, durations, bar onsets, staff/part counts, clef/key/time parity,
//! and valid `.mscz` + MusicXML outputs.

#![cfg(feature = "bekern")]

use mscz::{
    bekern::stitch_pages_bekern,
    decode::{decode_bekern, decode_bekern_unmerged},
    has_errors,
    musicxml_import::{score_input_from_musicxml, staff_headers},
    pack_mscz, parse_score, pitch, read_mscz, to_mscx, to_musicxml, validate_mscz,
    validate_musicxml, ScoreInput,
};

fn pitched_signature(input: &ScoreInput) -> Vec<(i32, i64, i64)> {
    let mut out = Vec::new();
    for part in &input.parts {
        for n in &part.notes {
            if n.rest || n.grace {
                continue;
            }
            let Some(p_str) = n.pitch.as_deref() else {
                continue;
            };
            let parsed = pitch::parse_pitch(p_str).expect("valid pitch");
            let midi = pitch::pitch_to_midi(parsed.step, parsed.alter, parsed.octave);
            out.push((
                midi,
                (n.onset * 480.0).round() as i64,
                (n.dur * 480.0).round() as i64,
            ));
        }
    }
    out.sort();
    out
}

fn compile_and_validate(bekern: &str, title: &str) -> (ScoreInput, String, String, Vec<u8>) {
    let mut decoded = decode_bekern_unmerged(bekern).expect("decode_bekern_unmerged");
    decoded.title = Some(title.to_string());
    let (score, mut losses) = parse_score(&decoded).expect("parse_score");
    let xml = to_musicxml(&score, &mut losses);
    let mscx = to_mscx(&score, &mut losses);
    let mscz_bytes = pack_mscz(&score, &mscx).expect("pack_mscz");

    let xml_issues = validate_musicxml(&xml);
    assert!(
        !has_errors(&xml_issues),
        "{title}: MusicXML validation errors: {xml_issues:?}"
    );
    let mscz_issues = validate_mscz(&mscz_bytes);
    assert!(
        !has_errors(&mscz_issues),
        "{title}: MSCZ validation errors: {mscz_issues:?}"
    );

    // Re-import generated MusicXML and verify exact note-for-note parity against merged decode.
    let expected = decode_bekern(bekern).expect("decode_bekern");
    let reimported = score_input_from_musicxml(&xml).expect("reimport MusicXML");
    assert_eq!(
        pitched_signature(&expected),
        pitched_signature(&reimported),
        "{title}: MusicXML round-trip note parity mismatch"
    );

    // Verify MSCZ unpacks cleanly and is deterministic.
    let contents = read_mscz(&mscz_bytes).expect("read_mscz");
    assert_eq!(contents.mscx, mscx, "{title}: mscz inner mscx mismatch");

    let (score2, mut losses2) = parse_score(&decoded).expect("parse_score 2");
    let mscx2 = to_mscx(&score2, &mut losses2);
    let mscz2 = pack_mscz(&score2, &mscx2).expect("pack_mscz 2");
    assert_eq!(mscz_bytes, mscz2, "{title}: .mscz must be byte-identical");

    (decoded, xml, mscx, mscz_bytes)
}

#[test]
fn parity_single_staff_clefs_and_octave_transposition() {
    for (clef_tok, exp_sign, exp_line, exp_oct, exp_ms4) in [
        ("clef-G-2", 'G', 2, None, "<concertClefType>G</concertClefType>"),
        ("clef-F-4", 'F', 4, None, "<concertClefType>F</concertClefType>"),
        ("clef-C-3", 'C', 3, None, "<concertClefType>C3</concertClefType>"),
        (
            "clef-G-2-8vb",
            'G',
            2,
            Some(-1),
            "<concertClefType>G8vb</concertClefType>",
        ),
        (
            "clef-G-2-8va",
            'G',
            2,
            Some(1),
            "<concertClefType>G8va</concertClefType>",
        ),
    ] {
        let bekern = format!(
            "{clef_tok} key-2 time-4-4 quarter B 3 a-1 quarter C 4 quarter D 4 quarter E 4 a-1 bar half F 4 half B 3 a-1 bar-light-heavy"
        );
        let (dec, xml, mscx, _) = compile_and_validate(&bekern, clef_tok);
        assert_eq!(dec.parts.len(), 1);
        let hdrs = staff_headers(&xml).expect("staff_headers");
        assert_eq!(hdrs.len(), 1);
        assert_eq!(
            (hdrs[0].clef_sign, hdrs[0].clef_line, hdrs[0].clef_octave_change),
            (exp_sign, exp_line, exp_oct)
        );
        assert_eq!(hdrs[0].key_fifths, -2);
        assert_eq!((hdrs[0].beats, hdrs[0].beat_type), (4, 4));
        assert!(mscx.contains(exp_ms4), "{clef_tok}: missing {exp_ms4}");
    }
}

#[test]
fn parity_two_staff_piano_grand_staff_and_polyphony() {
    // 2 staves (G-2, F-4), measure 1 has 2 voices (1 per staff), measure 2 has 3 voices (RH 2 + LH 1).
    let bekern = "\
        clef-G-2 key+1 time-4-4 clef-F-4 key+1 time-4-4 \
        voice-1 quarter G 4 quarter A 4 quarter B 4 quarter C 5 \
        voice-2 half G 2 half D 3 bar \
        voice-1 half D 5 half E 5 \
        voice-2 quarter B 4 quarter A 4 quarter G 4 quarter F 4 a1 \
        voice-3 whole G 2 bar-light-heavy";

    let (dec, xml, mscx, _) = compile_and_validate(bekern, "Piano Grand Staff");
    assert_eq!(dec.parts.len(), 2, "must emit 2 staves/parts");

    let hdrs = staff_headers(&xml).expect("staff_headers");
    assert_eq!(hdrs.len(), 2);
    assert_eq!((hdrs[0].clef_sign, hdrs[0].clef_line), ('G', 2));
    assert_eq!((hdrs[1].clef_sign, hdrs[1].clef_line), ('F', 4));
    assert_eq!(hdrs[0].key_fifths, 1);
    assert_eq!(hdrs[1].key_fifths, 1);

    assert!(mscx.contains("<Staff id=\"1\">"));
    assert!(mscx.contains("<Staff id=\"2\">"));
    assert!(mscx.contains("<concertClefType>G</concertClefType>"));
    assert!(mscx.contains("<concertClefType>F</concertClefType>"));
}

#[test]
fn parity_three_staff_voice_and_piano() {
    // 3 staves: Voice (G-2), Piano RH (G-2), Piano LH (F-4) with 4 voices in m2.
    let bekern = "\
        clef-G-2 key-1 time-3-4 clef-G-2 key-1 time-3-4 clef-F-4 key-1 time-3-4 \
        voice-1 quarter F 4 half A 4 \
        voice-2 quarter C 4 chord quarter F 4 half C 4 chord half A 4 \
        voice-3 half F 2 quarter C 3 bar \
        voice-1 half C 5 quarter A 4 \
        voice-2 half A 4 quarter F 4 \
        voice-3 half C 4 quarter C 4 \
        voice-4 half F 2 quarter F 2 bar-light-heavy";

    let (dec, xml, mscx, _) = compile_and_validate(bekern, "Voice + Piano");
    assert_eq!(dec.parts.len(), 3);

    let hdrs = staff_headers(&xml).expect("staff_headers");
    assert_eq!(hdrs.len(), 3);
    assert_eq!(
        hdrs.iter()
            .map(|h| (h.clef_sign, h.clef_line))
            .collect::<Vec<_>>(),
        vec![('G', 2), ('G', 2), ('F', 4)]
    );
    assert!(mscx.contains("<Staff id=\"1\">"));
    assert!(mscx.contains("<Staff id=\"2\">"));
    assert!(mscx.contains("<Staff id=\"3\">"));
}

#[test]
fn parity_four_staff_string_quartet_and_five_voice_split() {
    // 4 staves: Vln1 (G-2), Vln2 (G-2), Vla (C-3), Vc (F-4).
    // Measure 2 has 5 voice segments -> must split cleanly with <= 4 voices per staff!
    let bekern = "\
        clef-G-2 key+3 time-4-4 clef-G-2 key+3 time-4-4 clef-C-3 key+3 time-4-4 clef-F-4 key+3 time-4-4 \
        voice-1 whole E 5 \
        voice-2 whole C 5 a1 \
        voice-3 whole A 3 \
        voice-4 whole A 2 bar \
        voice-1 half F 5 a1 half E 5 \
        voice-2 half D 5 half C 5 a1 \
        voice-3 half A 4 half A 4 \
        voice-4 half D 4 half E 4 \
        voice-5 whole A 2 bar-light-heavy";

    let (dec, xml, mscx, _) = compile_and_validate(bekern, "String Quartet");
    assert_eq!(dec.parts.len(), 4);

    let hdrs = staff_headers(&xml).expect("staff_headers");
    assert_eq!(hdrs.len(), 4);
    assert_eq!(
        hdrs.iter()
            .map(|h| (h.clef_sign, h.clef_line))
            .collect::<Vec<_>>(),
        vec![('G', 2), ('G', 2), ('C', 3), ('F', 4)]
    );
    assert!(mscx.contains("<Staff id=\"4\">"));
    assert!(mscx.contains("<concertClefType>C3</concertClefType>"));
    assert!(mscx.contains("<concertClefType>F</concertClefType>"));
}

#[test]
fn parity_tuplets_emit_exact_time_modification_and_mscx_tuplet() {
    // Triplet eighths + triplet quarter+eighth + quintuplet 16ths in 4/4.
    let bekern = "\
        clef-G-2 key+0 time-4-4 \
        tuplet-start-3-2 eighth C 4 eighth D 4 eighth E 4 tuplet-stop \
        tuplet-start-3-2 quarter F 4 eighth G 4 tuplet-stop \
        tuplet-start-5-4 16th A 4 16th B 4 16th C 5 16th D 5 16th E 5 tuplet-stop \
        quarter C 5 bar-light-heavy";

    let (dec, xml, mscx, _) = compile_and_validate(bekern, "Tuplets");
    assert_eq!(dec.parts.len(), 1);
    assert!(
        xml.contains("<time-modification><actual-notes>3</actual-notes><normal-notes>2</normal-notes></time-modification>"),
        "MusicXML must emit triplet <time-modification>"
    );
    assert!(
        xml.contains("<time-modification><actual-notes>5</actual-notes><normal-notes>4</normal-notes></time-modification>"),
        "MusicXML must emit quintuplet <time-modification>"
    );
    assert!(
        mscx.contains("<Tuplet>"),
        "MSCX must emit <Tuplet> for non-dyadic tuplet durations"
    );
    assert_eq!(
        mscx.matches("<Tuplet>").count(),
        mscx.matches("<endTuplet/>").count(),
        "every <Tuplet> must have a matching <endTuplet/>"
    );
    assert_eq!(mscx.matches("<Tuplet>").count(), 3);
}

#[test]
fn parity_repeats_double_accidentals_dynamics_hairpins_and_midstream_changes() {
    let bekern = "\
        clef-G-2 key+0 time-4-4 \
        bar-repeat-left dyn-p hairpin-cresc-start \
        quarter F 4 a2 quarter B 4 a-2 half C 5 slur-start stacc bar-repeat-right \
        clef-F-4 key-3 time-3-4 dyn-f hairpin-stop \
        quarter G 3 slur-stop accent quarter A 3 a-1 tenuto quarter B 3 a-1 fermata bar-double \
        dyn-sfz half C 3 quarter C 3 bar-light-heavy";

    let (dec, xml, mscx, _) = compile_and_validate(bekern, "Rich Notation");
    assert_eq!(dec.parts.len(), 1);
    // Double accidentals
    assert!(xml.contains("<alter>2</alter>"));
    assert!(xml.contains("<accidental>double-sharp</accidental>"));
    assert!(xml.contains("<alter>-2</alter>"));
    assert!(xml.contains("<accidental>flat-flat</accidental>"));
    // Repeats & barlines
    assert!(xml.contains(r#"<repeat direction="forward"/>"#));
    assert!(xml.contains(r#"<repeat direction="backward"/>"#));
    assert!(xml.contains("<bar-style>light-light</bar-style>"));
    assert!(xml.contains("<bar-style>light-heavy</bar-style>"));
    assert!(mscx.contains("<startRepeat/>"));
    assert!(mscx.contains("<endRepeat>2</endRepeat>"));
    assert!(mscx.contains("<subtype>double</subtype>"));
    assert!(mscx.contains("<subtype>end</subtype>"));
    // Dynamics & hairpins
    assert!(xml.contains("<p/>") && xml.contains("<f/>") && xml.contains("<sfz/>"));
    assert!(xml.contains(r#"<wedge type="crescendo"/>"#));
    assert!(mscx.contains("<Spanner type=\"HairPin\">"));
    // Mid-stream clef / key / time changes
    assert!(xml.contains("<sign>F</sign><line>4</line>"));
    assert!(xml.contains("<fifths>-3</fifths>"));
    assert!(xml.contains("<beats>3</beats><beat-type>4</beat-type>"));
    assert!(mscx.contains("<concertClefType>F</concertClefType>"));
    assert!(mscx.contains("<concertKey>-3</concertKey>"));
    assert!(mscx.contains("<sigN>3</sigN>"));
}

#[test]
fn parity_multipage_stitch_pages_bekern_preserves_multistaff_headers() {
    let page1 = "<bos> clef-G-2 key-1 time-4-4 clef-F-4 key-1 time-4-4 voice-1 whole F 4 voice-2 whole F 2 bar <eos>";
    // Page 2 omits time signature (standard engraving on page 2+)
    let page2 = "<bos> clef-G-2 key-1 clef-F-4 key-1 voice-1 half A 4 half C 5 voice-2 half C 3 half F 2 bar-light-heavy <eos>";

    let stitched = stitch_pages_bekern(&[page1, page2]);
    assert!(stitched.contains("<sys>"));
    // Compile through the WASM-style single-entry stitcher when wasm feature is enabled,
    // or strip <sys> via manual check; verify both decode cleanly with 2 staves.
    let single_stream = stitched.replace(" <sys> clef-G-2 key-1 clef-F-4 key-1 time-4-4 ", " ");
    let (dec, xml, mscx, _) = compile_and_validate(&single_stream, "Multi-Page Piano");
    assert_eq!(dec.parts.len(), 2);
    let hdrs = staff_headers(&xml).expect("staff_headers");
    assert_eq!(hdrs.len(), 2);
    assert_eq!((hdrs[0].clef_sign, hdrs[0].clef_line), ('G', 2));
    assert_eq!((hdrs[1].clef_sign, hdrs[1].clef_line), ('F', 4));
    assert_eq!(hdrs[0].key_fifths, -1);
    assert!(mscx.contains("<Staff id=\"2\">"));
}

#[cfg(feature = "wasm")]
#[test]
fn parity_wasm_multistaff_sys_stitch_and_compile() {
    use mscz::wasm::{compile_bekern_to_mscz, compile_bekern_to_musicxml, stitch_uni_bekern};

    let raw = "<bos> clef-G-2 key+2 time-3-4 clef-F-4 key+2 time-3-4 voice-1 half D 4 quarter F 4 a1 voice-2 half D 2 quarter A 2 \
               <sys> clef-G-2 key+2 time-3-4 clef-F-4 key+2 time-3-4 voice-1 half A 4 quarter D 5 voice-2 half D 2 quarter D 2 bar-light-heavy <eos>";
    let stitched = stitch_uni_bekern(raw);
    // Courtesy clef-G-2 / clef-F-4 on system 2 must be stripped, not emitted as mid-stream clefs!
    assert_eq!(stitched.matches("clef-G-2").count(), 1);
    assert_eq!(stitched.matches("clef-F-4").count(), 1);

    let xml = compile_bekern_to_musicxml(&stitched, Some("WASM Piano")).expect("wasm xml");
    assert!(!has_errors(&validate_musicxml(&xml)));
    let hdrs = staff_headers(&xml).expect("staff_headers");
    assert_eq!(hdrs.len(), 2);
    assert_eq!((hdrs[0].clef_sign, hdrs[0].clef_line), ('G', 2));
    assert_eq!((hdrs[1].clef_sign, hdrs[1].clef_line), ('F', 4));

    let mscz_bytes = compile_bekern_to_mscz(&stitched, Some("WASM Piano")).expect("wasm mscz");
    assert!(!has_errors(&validate_mscz(&mscz_bytes)));
    let contents = read_mscz(&mscz_bytes).expect("read_mscz");
    assert!(contents.mscx.contains("<Staff id=\"1\">"));
    assert!(contents.mscx.contains("<Staff id=\"2\">"));
    // Verify losses table is clean when compiled via parse_score directly.
    let decoded = decode_bekern_unmerged(&stitched).expect("decode");
    let (score, mut losses) = parse_score(&decoded).expect("parse");
    let _ = to_musicxml(&score, &mut losses);
    assert_eq!(losses.loss_count(), 0);
}
