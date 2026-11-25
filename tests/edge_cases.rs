//! Untrusted-input security and edge-case hardening tests.

use std::io::{Cursor, Write};

use mscz::{
    has_errors, is_empty_score_input, pack_mscz_bytes, parse_score, read_mscz, to_musicxml,
    validate_mscz, validate_musicxml, NoteInput, PartInput, ScoreInput, MAX_MSCZ_ENTRY_BYTES,
};

// ---------------------------------------------------------------------------
// 1. Untrusted / malformed MusicXML + multi-byte UTF-8
// ---------------------------------------------------------------------------

#[test]
fn validate_musicxml_untrusted_malformed_and_utf8_no_panic() {
    let cases = [
        "",
        "not xml at all",
        "<?xml version=\"1.0\"?>",
        "<score-partwise version=\"3.1\">",
        // Multi-byte UTF-8 right around `<score-partwise` (+200 byte window) and `&` (+8 byte window)
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<score-partwise version=\"3.1\"><!-- 🎼🎵🎶🎹🎻🎺🎸🥁🎷 — “curly quotes” &amp; Ümläuts öäüß naïve café résumé 日本語 한국어 -->",
        // Unescaped `&` immediately followed by multi-byte UTF-8 emojis/umlauts/curly quotes
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<score-partwise version=\"3.1\"><work><work-title>&🎼</work-title></work></score-partwise>",
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<score-partwise version=\"3.1\"><work><work-title>&“ö”</work-title></work></score-partwise>",
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<score-partwise version=\"3.1\"><work><work-title>&",
        // Unclosed <measure>
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<score-partwise version=\"3.1\"><part id=\"P1\"><measure number=\"1\"><note><pitch><step>C</step><octave>4</octave></pitch><duration>4</duration></note>",
        // Adversarial overflow values in divisions / beats / duration
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<score-partwise version=\"3.1\"><part id=\"P1\"><measure number=\"1\"><attributes><divisions>9223372036854775807</divisions><time><beats>9223372036854775807</beats><beat-type>4</beat-type></time></attributes><note><pitch><step>C</step><octave>4</octave></pitch><duration>9223372036854775807</duration></note><note><pitch><step>D</step><octave>4</octave></pitch><duration>9223372036854775807</duration></note></measure></part></score-partwise>",
        // Garbage tags with multi-byte UTF-8 right after `<`
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<score-partwise version=\"3.1\"><part id=\"P1\"><measure number=\"1\"><🎼/><ü><“note”><duration>bad</duration></measure></part></score-partwise>",
    ];

    for (idx, xml) in cases.iter().enumerate() {
        let issues = validate_musicxml(xml);
        assert!(
            !issues.is_empty(),
            "case {idx} should produce at least one validation issue"
        );
    }
}

#[test]
fn validate_musicxml_valid_multibyte_utf8_and_note_attrs() {
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<score-partwise version="3.1">
  <work><work-title>Frühlingslied — “Café” 🎼 &amp; naïve</work-title></work>
  <part-list><score-part id="P1"><part-name>Sänger 🎵</part-name></score-part></part-list>
  <part id="P1">
    <measure number="1">
      <attributes>
        <divisions>4</divisions>
        <time><beats>4</beats><beat-type>4</beat-type></time>
      </attributes>
      <note default-x="10">
        <pitch><step>C</step><octave>4</octave></pitch>
        <duration>8</duration>
        <lyric><text>Schön “la” 🎶</text></lyric>
      </note>
      <note default-x="42.5">
        <pitch><step>G</step><octave>4</octave></pitch>
        <duration>8</duration>
      </note>
    </measure>
  </part>
</score-partwise>"#;

    let issues = validate_musicxml(xml);
    assert!(
        !has_errors(&issues),
        "valid UTF-8 + attributed <note> should have no errors: {issues:?}"
    );
}

#[test]
fn validate_musicxml_unclosed_note_and_attributed_note_missing_pitch() {
    // Unclosed <note> at end of measure must not panic.
    let unclosed = r#"<?xml version="1.0" encoding="UTF-8"?>
<score-partwise version="3.1">
  <part id="P1">
    <measure number="1">
      <attributes><divisions>1</divisions><time><beats>1</beats><beat-type>4</beat-type></time></attributes>
      <note default-x="10"><pitch><step>C</step><octave>4</octave></pitch><duration>1</duration>
    </measure>
  </part>
</score-partwise>"#;
    let _ = validate_musicxml(unclosed);

    // <note default-x="10"> without <step>/<octave> or <rest/> must be caught.
    let missing_pitch = r#"<?xml version="1.0" encoding="UTF-8"?>
<score-partwise version="3.1">
  <part id="P1">
    <measure number="1">
      <attributes><divisions>1</divisions><time><beats>1</beats><beat-type>4</beat-type></time></attributes>
      <note default-x="10"><duration>1</duration></note>
    </measure>
  </part>
</score-partwise>"#;
    let issues = validate_musicxml(missing_pitch);
    assert!(
        issues.iter().any(|i| i.code == "xml-pitch"),
        "expected xml-pitch error for attributed <note> missing pitch, got {issues:?}"
    );
}

// ---------------------------------------------------------------------------
// 2. Pickup <Measure len="1/4"> validation in .mscz
// ---------------------------------------------------------------------------

#[test]
fn validate_mscz_accepts_pickup_measure_with_len_attribute() {
    let mscx = r#"<?xml version="1.0" encoding="UTF-8"?>
<museScore version="4.20">
  <Score>
    <Staff id="1">
      <Measure len="1/4">
        <voice>
          <Chord>
            <durationType>quarter</durationType>
            <Note><pitch>60</pitch><tpc>14</tpc></Note>
          </Chord>
        </voice>
      </Measure>
    </Staff>
  </Score>
</museScore>
"#;
    let bytes = build_raw_mscz("pickup.mscx", mscx.as_bytes(), None);
    let issues = validate_mscz(&bytes);
    assert!(
        !has_errors(&issues),
        "pickup <Measure len=\"1/4\"> should validate cleanly, got {issues:?}"
    );
    let contents = read_mscz(&bytes).expect("read_mscz should succeed on pickup measure");
    assert!(contents.mscx.contains("<Measure len=\"1/4\">"));
}

// ---------------------------------------------------------------------------
// 3. parse_score numeric & structural hardening
// ---------------------------------------------------------------------------

#[test]
fn parse_score_rejects_non_finite_negative_zero_and_huge_values() {
    let bad_notes = [
        ("NaN onset", NoteInput::note("C4", f64::NAN, 1.0)),
        ("NaN dur", NoteInput::note("C4", 0.0, f64::NAN)),
        ("+Infinity onset", NoteInput::note("C4", f64::INFINITY, 1.0)),
        (
            "-Infinity onset",
            NoteInput::note("C4", f64::NEG_INFINITY, 1.0),
        ),
        ("+Infinity dur", NoteInput::note("C4", 0.0, f64::INFINITY)),
        (
            "-Infinity dur",
            NoteInput::note("C4", 0.0, f64::NEG_INFINITY),
        ),
        ("negative onset", NoteInput::note("C4", -0.25, 1.0)),
        ("zero dur", NoteInput::note("C4", 0.0, 0.0)),
        ("negative dur", NoteInput::note("C4", 0.0, -1.0)),
        ("huge onset 1e15", NoteInput::note("C4", 1e15, 1.0)),
        ("huge dur 1e15", NoteInput::note("C4", 0.0, 1e15)),
        ("onset just over 1e7", NoteInput::note("C4", 1e7 + 1.0, 1.0)),
        ("dur just over 1e7", NoteInput::note("C4", 0.0, 1e7 + 1.0)),
        (
            "tiny dur rounding to 0 ticks",
            NoteInput::note("C4", 0.0, 1e-12),
        ),
    ];

    for (label, note) in bad_notes {
        let input = ScoreInput {
            notes: vec![note],
            ..Default::default()
        };
        let res = parse_score(&input);
        assert!(res.is_err(), "expected Err for {label}, got Ok");
    }
}

// ---------------------------------------------------------------------------
// 4. Truncated / corrupted zip bytes & zip-bomb size guard
// ---------------------------------------------------------------------------

#[test]
fn read_and_validate_mscz_handle_truncated_and_corrupted_bytes() {
    // Build a real valid .mscz, then test every prefix truncation and byte corruption.
    let input = ScoreInput {
        title: Some("Corrupt Test".into()),
        notes: vec![NoteInput::note("C4", 0.0, 4.0)],
        ..Default::default()
    };
    let (score, mut losses) = parse_score(&input).unwrap();
    let valid = pack_mscz_bytes(&score, &mut losses).unwrap();
    assert!(!has_errors(&validate_mscz(&valid)));

    // Empty and garbage inputs
    for garbage in [
        &b""[..],
        &b"PK"[..],
        &b"PK\x03\x04"[..],
        &b"PK\x03\x04\x14\x00\x00\x00\x08\x00"[..],
        &b"not a zip file at all"[..],
    ] {
        assert!(read_mscz(garbage).is_err());
        assert!(has_errors(&validate_mscz(garbage)));
    }

    // Truncated at various offsets
    for cut in [1, 16, valid.len() / 3, valid.len() / 2, valid.len() - 1] {
        let slice = &valid[..cut];
        assert!(
            read_mscz(slice).is_err(),
            "truncated zip at {cut} bytes should fail read_mscz"
        );
        assert!(
            has_errors(&validate_mscz(slice)),
            "truncated zip at {cut} bytes should report error in validate_mscz"
        );
    }

    // Corrupted middle payload bytes
    let mut corrupted = valid.clone();
    let mid = corrupted.len() / 2;
    let end = (mid + 8).min(corrupted.len());
    for b in &mut corrupted[mid.saturating_sub(8)..end] {
        *b ^= 0xFF;
    }
    assert!(read_mscz(&corrupted).is_err());
    assert!(has_errors(&validate_mscz(&corrupted)));
}

#[test]
fn read_and_validate_mscz_reject_zip_bomb_entries() {
    let over_limit = (MAX_MSCZ_ENTRY_BYTES + 1024) as usize;
    let mscx_bomb = build_deflated_bomb_mscz("bomb.mscx", over_limit, None);
    let err = read_mscz(&mscx_bomb).expect_err("zip bomb .mscx must be rejected by read_mscz");
    assert!(
        err.contains("exceeds maximum uncompressed size"),
        "unexpected error message: {err}"
    );
    let issues = validate_mscz(&mscx_bomb);
    assert!(
        has_errors(&issues),
        "zip bomb .mscx must be rejected by validate_mscz: {issues:?}"
    );

    let mss_bomb = build_deflated_bomb_mscz("ok.mscx", 0, Some(("score_style.mss", over_limit)));
    let err = read_mscz(&mss_bomb).expect_err("zip bomb .mss must be rejected by read_mscz");
    assert!(
        err.contains("exceeds maximum uncompressed size"),
        "unexpected error message: {err}"
    );
    let issues = validate_mscz(&mss_bomb);
    assert!(
        has_errors(&issues),
        "zip bomb .mss must be rejected by validate_mscz: {issues:?}"
    );
}

// ---------------------------------------------------------------------------
// 5. Empty score, empty part, and microtone flat preservation
// ---------------------------------------------------------------------------

#[test]
fn empty_score_and_empty_part_handling() {
    // Completely empty score
    let empty = ScoreInput::default();
    assert!(is_empty_score_input(&empty));
    assert!(parse_score(&empty).is_err());

    // Score with only empty parts
    let all_empty_parts = ScoreInput {
        parts: vec![
            PartInput {
                name: "Violin".into(),
                notes: vec![],
                ..Default::default()
            },
            PartInput {
                name: "Cello".into(),
                notes: vec![],
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    assert!(is_empty_score_input(&all_empty_parts));
    assert!(parse_score(&all_empty_parts).is_err());

    // Score with one empty part and one populated part: empty part is skipped cleanly
    let mixed = ScoreInput {
        parts: vec![
            PartInput {
                name: "Silent".into(),
                notes: vec![],
                ..Default::default()
            },
            PartInput {
                name: "Piano".into(),
                notes: vec![NoteInput::note("C4", 0.0, 4.0)],
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    assert!(!is_empty_score_input(&mixed));
    let (score, mut losses) = parse_score(&mixed).expect("mixed parts should parse");
    assert_eq!(score.parts.len(), 1);
    assert_eq!(score.parts[0].name, "Piano");
    let xml = to_musicxml(&score, &mut losses);
    assert!(!has_errors(&validate_musicxml(&xml)));
}

#[test]
fn microtone_flat_spelling_preserved_in_parse_and_musicxml() {
    let input = ScoreInput {
        notes: vec![NoteInput::note("Bb4+10c", 0.0, 4.0)],
        ..Default::default()
    };
    let (score, mut losses) = parse_score(&input).expect("Bb4+10c should parse");
    let note = &score.parts[0].groups[0].members[0];
    assert_eq!(note.step, 'B');
    assert_eq!(note.alter, -1);
    assert_eq!(note.octave, 4);
    assert_eq!(note.midi, 70);
    assert!(
        losses
            .entries
            .iter()
            .any(|e| e.kind == "pitch-microtone-rounded"),
        "expected microtone rounding loss entry"
    );

    let xml = to_musicxml(&score, &mut losses);
    assert!(xml.contains("<step>B</step>"));
    assert!(xml.contains("<alter>-1</alter>"));
    assert!(xml.contains("<octave>4</octave>"));
    assert!(!has_errors(&validate_musicxml(&xml)));
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn build_raw_mscz(mscx_name: &str, mscx_bytes: &[u8], style: Option<(&str, &[u8])>) -> Vec<u8> {
    let mut buf = Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        let style_root = style
            .map(|(name, _)| format!("\n    <rootfile full-path=\"{name}\"/>"))
            .unwrap_or_default();
        let container = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<container>\n  <rootfiles>\n    <rootfile full-path=\"{mscx_name}\"/>{style_root}\n  </rootfiles>\n</container>\n"
        );
        zip.start_file("META-INF/container.xml", opts).unwrap();
        zip.write_all(container.as_bytes()).unwrap();
        zip.start_file(mscx_name, opts).unwrap();
        zip.write_all(mscx_bytes).unwrap();
        if let Some((name, data)) = style {
            zip.start_file(name, opts).unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap();
    }
    buf.into_inner()
}

fn build_deflated_bomb_mscz(
    mscx_name: &str,
    mscx_bomb_bytes: usize,
    style_bomb: Option<(&str, usize)>,
) -> Vec<u8> {
    let mut buf = Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .compression_level(Some(1));
        let style_root = style_bomb
            .map(|(name, _)| format!("\n    <rootfile full-path=\"{name}\"/>"))
            .unwrap_or_default();
        let container = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<container>\n  <rootfiles>\n    <rootfile full-path=\"{mscx_name}\"/>{style_root}\n  </rootfiles>\n</container>\n"
        );
        zip.start_file("META-INF/container.xml", opts).unwrap();
        zip.write_all(container.as_bytes()).unwrap();

        let chunk = [b' '; 64 * 1024];
        zip.start_file(mscx_name, opts).unwrap();
        if mscx_bomb_bytes > 0 {
            let mut rem = mscx_bomb_bytes;
            while rem > 0 {
                let n = rem.min(chunk.len());
                zip.write_all(&chunk[..n]).unwrap();
                rem -= n;
            }
        } else {
            zip.write_all(
                b"<?xml version=\"1.0\"?><museScore version=\"4.20\"><Score><Staff id=\"1\"><Measure></Measure></Staff></Score></museScore>",
            )
            .unwrap();
        }

        if let Some((name, size)) = style_bomb {
            zip.start_file(name, opts).unwrap();
            let mut rem = size;
            while rem > 0 {
                let n = rem.min(chunk.len());
                zip.write_all(&chunk[..n]).unwrap();
                rem -= n;
            }
        }

        zip.finish().unwrap();
    }
    buf.into_inner()
}
