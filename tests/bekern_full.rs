//! Comprehensive 4-layer `bekern-full` (Iter 16 Pillar 1) emission, decoding,
//! MusicXML round-trip, and strict `v1`/`v2` backward-compatibility tests.

use mscz::{
    bekern::{to_bekern, to_bekern_full, to_bekern_v2, to_bekern_with_dialect, BekernDialect},
    decode::{decode_bekern, decode_bekern_lenient, decode_bekern_unmerged},
    has_errors,
    musicxml_import::{bekern_events, staff_headers},
    parse_score, to_musicxml, validate_musicxml,
};

const LAYER0_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<score-partwise version="3.1">
  <part-list>
    <part-group type="start" number="1"><group-symbol>bracket</group-symbol></part-group>
    <score-part id="P1"><part-name>Bb Clarinet</part-name></score-part>
    <part-group type="stop" number="1"/>
  </part-list>
  <part id="P1">
    <measure number="0" implicit="yes">
      <attributes>
        <divisions>4</divisions>
        <key><fifths>2</fifths></key>
        <time><beats>4</beats><beat-type>4</beat-type></time>
        <clef><sign>G</sign><line>2</line></clef>
        <staff-details><staff-lines>5</staff-lines></staff-details>
        <transpose><diatonic>-1</diatonic><chromatic>-2</chromatic></transpose>
      </attributes>
      <direction placement="above"><direction-type><segno/></direction-type></direction>
      <note><pitch><step>C</step><octave>4</octave></pitch><duration>4</duration><voice>1</voice><instrument id="P1-I1"/></note>
    </measure>
    <measure number="1">
      <attributes>
        <measure-style><multiple-rest>4</multiple-rest></measure-style>
      </attributes>
      <note><rest measure="yes"/><duration>16</duration><voice>1</voice></note>
    </measure>
    <measure number="2">
      <attributes>
        <measure-style><measure-repeat type="start">1</measure-repeat></measure-style>
      </attributes>
      <direction placement="above"><direction-type><coda/></direction-type></direction>
      <note><pitch><step>D</step><octave>4</octave></pitch><duration>16</duration><voice>1</voice><instrument id="P1-I2"/></note>
    </measure>
    <measure number="3">
      <attributes>
        <measure-style><measure-repeat type="start">2</measure-repeat></measure-style>
      </attributes>
      <direction placement="above"><direction-type><words>D.C.</words></direction-type><sound dacapo="yes"/></direction>
      <direction placement="above"><direction-type><words>D.S.</words></direction-type><sound dalsegno="yes"/></direction>
      <note><pitch><step>E</step><octave>4</octave></pitch><duration>16</duration><voice>1</voice></note>
    </measure>
    <measure number="4">
      <attributes>
        <measure-style><measure-repeat type="start">4</measure-repeat></measure-style>
      </attributes>
      <direction placement="above"><direction-type><words>Fine</words></direction-type><sound fine="yes"/></direction>
      <note><pitch><step>F</step><octave>4</octave></pitch><duration>16</duration><voice>1</voice></note>
    </measure>
  </part>
</score-partwise>"#;

#[test]
fn bekern_full_layer0_structure_multipart_navigation_roundtrip() {
    let headers = staff_headers(LAYER0_XML).unwrap();
    let events = bekern_events(LAYER0_XML).unwrap();
    let (toks, skipped) = to_bekern_full(&events, &headers).unwrap();
    assert_eq!(skipped, 0);
    assert_eq!(toks[0], "<schema:full>");

    let s = toks.join(" ");
    for expected in [
        "<group> bracket",
        "<part>",
        "<staff-def>",
        "<transpose> -1 -2",
        "<pickup>",
        "<multirest> multirest-4",
        "mrep-1",
        "mrep-2",
        "mrep-4",
        "<inst-change>",
        "Segno",
        "Coda",
        "D.C.",
        "D.S.",
        "Fine",
    ] {
        assert!(
            s.contains(expected),
            "missing Layer 0 token {expected:?} in: {s}"
        );
    }

    // Round-trip decode -> parse_score -> to_musicxml
    let input = decode_bekern_unmerged(&s).expect("decode Layer 0 full stream");
    assert_eq!(
        input.parts[0]
            .full
            .as_deref()
            .and_then(|f| f.group_symbol.as_deref()),
        Some("bracket")
    );
    assert_eq!(
        input.parts[0].full.as_deref().and_then(|f| f.transpose),
        Some((-1, -2))
    );
    let (score, mut losses) = parse_score(&input).expect("parse Layer 0 score");
    let xml = to_musicxml(&score, &mut losses);
    assert!(xml.contains("<group-symbol>bracket</group-symbol>"));
    assert!(xml.contains("<transpose><diatonic>-1</diatonic><chromatic>-2</chromatic></transpose>"));
    assert!(xml.contains("implicit=\"yes\""));
    assert!(xml.contains("<multiple-rest>4</multiple-rest>"));
    assert!(xml.contains("<measure-repeat type=\"start\">1</measure-repeat>"));
    assert!(xml.contains("<measure-repeat type=\"start\">2</measure-repeat>"));
    assert!(xml.contains("<measure-repeat type=\"start\">4</measure-repeat>"));
    assert!(xml.contains("<segno/>"));
    assert!(xml.contains("<coda/>"));
    assert!(xml.contains("dacapo=\"yes\""));
    assert!(xml.contains("dalsegno=\"yes\""));
    assert!(xml.contains("fine=\"yes\""));
    let issues = validate_musicxml(&xml);
    assert!(!has_errors(&issues), "Layer 0 MusicXML errors: {issues:?}");
}

const LAYER1_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<score-partwise version="3.1">
  <part-list>
    <score-part id="P1"><part-name>Extended</part-name></score-part>
  </part-list>
  <part id="P1">
    <measure number="1">
      <attributes>
        <divisions>44</divisions>
        <staves>2</staves>
        <key><fifths>0</fifths></key>
        <time><beats>4</beats><beat-type>4</beat-type></time>
        <clef number="1"><sign>G</sign><line>2</line></clef>
        <clef number="2"><sign>F</sign><line>4</line></clef>
      </attributes>
      <note>
        <grace slash="yes"/>
        <pitch><step>B</step><octave>4</octave></pitch>
        <voice>1</voice><type>eighth</type><staff>1</staff>
      </note>
      <note>
        <pitch><step>C</step><alter>0.5</alter><octave>4</octave></pitch>
        <duration>22</duration><voice>1</voice><type>eighth</type>
        <accidental cautionary="yes">quarter-sharp</accidental>
        <stem>up</stem><staff>1</staff><beam number="1">begin</beam>
      </note>
      <note>
        <chord/>
        <pitch><step>E</step><alter>-0.5</alter><octave>4</octave></pitch>
        <duration>22</duration><voice>1</voice><type>eighth</type>
        <accidental parentheses="yes">quarter-flat</accidental>
        <staff>1</staff>
      </note>
      <note>
        <pitch><step>F</step><alter>1.5</alter><octave>4</octave></pitch>
        <duration>22</duration><voice>1</voice><type>eighth</type>
        <stem>down</stem><staff>1</staff><beam number="1">continue</beam>
      </note>
      <note>
        <pitch><step>G</step><alter>-1.5</alter><octave>4</octave></pitch>
        <duration>22</duration><voice>1</voice><type>eighth</type>
        <staff>1</staff><beam number="1">end</beam>
      </note>
      <note>
        <pitch><step>A</step><octave>4</octave></pitch>
        <duration>22</duration><voice>1</voice><type>eighth</type>
        <accidental>natural</accidental>
        <staff>1</staff><beam number="1">backward hook</beam>
        <notations><technical><string>2</string><fret>5</fret><bend><bend-alter>2</bend-alter></bend><hammer-on type="start"/></technical></notations>
      </note>
      <note>
        <pitch><step>B</step><octave>4</octave></pitch>
        <duration>22</duration><voice>1</voice><type>eighth</type>
        <staff>2</staff><beam number="1">forward hook</beam>
      </note>
      <note>
        <unpitched><display-step>C</display-step><display-octave>5</display-octave></unpitched>
        <duration>44</duration><voice>1</voice><type>quarter</type>
        <notehead>x</notehead><staff>1</staff>
      </note>
    </measure>
    <measure number="2">
      <note>
        <pitch><step>C</step><octave>4</octave></pitch>
        <duration>8</duration><voice>1</voice><type>16th</type>
        <time-modification><actual-notes>11</actual-notes><normal-notes>8</normal-notes></time-modification>
        <staff>1</staff>
        <notations><tuplet type="start" number="1"/></notations>
      </note>
      <note>
        <pitch><step>D</step><octave>4</octave></pitch>
        <duration>8</duration><voice>1</voice><type>16th</type>
        <time-modification><actual-notes>11</actual-notes><normal-notes>8</normal-notes></time-modification>
        <staff>1</staff>
        <notations><tuplet type="stop" number="1"/></notations>
      </note>
      <note><unpitched><display-step>E</display-step><display-octave>5</display-octave></unpitched><duration>44</duration><voice>1</voice><type>quarter</type><notehead>diamond</notehead><staff>1</staff></note>
      <note><unpitched><display-step>F</display-step><display-octave>5</display-octave></unpitched><duration>44</duration><voice>1</voice><type>quarter</type><notehead>slash</notehead><staff>1</staff></note>
      <note><unpitched><display-step>G</display-step><display-octave>5</display-octave></unpitched><duration>22</duration><voice>1</voice><type>eighth</type><notehead>triangle</notehead><staff>1</staff></note>
      <note><unpitched><display-step>A</display-step><display-octave>5</display-octave></unpitched><duration>22</duration><voice>1</voice><type>eighth</type><notehead>circle-x</notehead><staff>1</staff></note>
      <note><unpitched><display-step>B</display-step><display-octave>5</display-octave></unpitched><duration>22</duration><voice>1</voice><type>eighth</type><notehead>square</notehead><staff>1</staff></note>
    </measure>
  </part>
</score-partwise>"#;

#[test]
fn bekern_full_layer1_core_extensions_roundtrip() {
    let headers = staff_headers(LAYER1_XML).unwrap();
    let events = bekern_events(LAYER1_XML).unwrap();
    let (toks, _) = to_bekern_full(&events, &headers).unwrap();
    let s = toks.join(" ");

    for expected in [
        "<schema:full>",
        "grace-slash",
        "q#",
        "qb",
        "3q#",
        "3qb",
        "a0",
        "acc-caut",
        "acc-paren",
        "<chord>",
        "stem-up",
        "stem-down",
        "beam-start",
        "beam-cont",
        "beam-end",
        "beam-hook-l",
        "beam-hook-r",
        "unpitched",
        "nh-x",
        "nh-diamond",
        "nh-slash",
        "nh-triangle",
        "nh-circle-x",
        "nh-square",
        "str-2",
        "fret-5",
        "bend",
        "hopo",
        "xstaff-2",
        "tup-open-11:8",
        "tup-close",
    ] {
        assert!(
            s.contains(expected),
            "missing Layer 1 token {expected:?} in: {s}"
        );
    }

    let input = decode_bekern_unmerged(&s).expect("decode Layer 1 full stream");
    let (score, mut losses) = parse_score(&input).expect("parse Layer 1 score");
    let xml = to_musicxml(&score, &mut losses);
    assert!(xml.contains("<grace slash=\"yes\"/>"));
    assert!(xml.contains("<alter>0.5</alter>"));
    assert!(xml.contains("<alter>-0.5</alter>"));
    assert!(xml.contains("<alter>1.5</alter>"));
    assert!(xml.contains("<alter>-1.5</alter>"));
    assert!(xml.contains("cautionary=\"yes\">quarter-sharp</accidental>"));
    assert!(xml.contains("parentheses=\"yes\">quarter-flat</accidental>"));
    assert!(xml.contains("<accidental>natural</accidental>"));
    assert!(xml.contains("<stem>up</stem>"));
    assert!(xml.contains("<stem>down</stem>"));
    assert!(xml.contains("<beam number=\"1\">begin</beam>"));
    assert!(xml.contains("<beam number=\"1\">continue</beam>"));
    assert!(xml.contains("<beam number=\"1\">end</beam>"));
    assert!(xml.contains("<beam number=\"1\">backward hook</beam>"));
    assert!(xml.contains("<beam number=\"1\">forward hook</beam>"));
    assert!(xml.contains("<unpitched><display-step>C</display-step><display-octave>5</display-octave></unpitched>"));
    assert!(xml.contains("<notehead>x</notehead>"));
    assert!(xml.contains("<notehead>diamond</notehead>"));
    assert!(xml.contains("<notehead>slash</notehead>"));
    assert!(xml.contains("<notehead>triangle</notehead>"));
    assert!(xml.contains("<notehead>circle-x</notehead>"));
    assert!(xml.contains("<notehead>square</notehead>"));
    assert!(xml.contains("<string>2</string><fret>5</fret>"));
    assert!(xml.contains("<bend>"));
    assert!(xml.contains("<hammer-on"));
    assert!(xml.contains("<actual-notes>11</actual-notes><normal-notes>8</normal-notes>"));
    let issues = validate_musicxml(&xml);
    assert!(!has_errors(&issues), "Layer 1 MusicXML errors: {issues:?}");
}

const LAYER2_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<score-partwise version="3.1">
  <part-list>
    <score-part id="P1"><part-name>Expressive</part-name></score-part>
  </part-list>
  <part id="P1">
    <measure number="1">
      <attributes>
        <divisions>4</divisions>
        <key><fifths>0</fifths></key>
        <time><beats>4</beats><beat-type>4</beat-type></time>
        <clef><sign>G</sign><line>2</line></clef>
      </attributes>
      <direction placement="above"><direction-type><octave-shift type="down" size="15"/></direction-type></direction>
      <direction placement="below"><direction-type><pedal type="start" sostenuto="yes"/></direction-type></direction>
      <direction placement="below"><direction-type><wedge type="crescendo" number="2"/></direction-type></direction>
      <note>
        <pitch><step>C</step><octave>5</octave></pitch>
        <duration>4</duration><voice>1</voice><type>quarter</type>
        <notations>
          <slur type="start" number="2"/>
          <articulations><strong-accent/><breath-mark/></articulations>
          <ornaments><inverted-mordent/><tremolo type="single">1</tremolo><wavy-line type="start"/></ornaments>
          <technical><fingering>1</fingering><harmonic/></technical>
          <arpeggiate direction="up"/>
          <glissando type="start"/>
        </notations>
      </note>
      <direction placement="above"><direction-type><octave-shift type="stop" size="15"/></direction-type></direction>
      <direction placement="below"><direction-type><pedal type="stop" sostenuto="yes"/></direction-type></direction>
      <direction placement="below"><direction-type><wedge type="stop" number="2"/></direction-type></direction>
      <note>
        <pitch><step>D</step><octave>5</octave></pitch>
        <duration>4</duration><voice>1</voice><type>quarter</type>
        <notations>
          <slur type="stop" number="2"/>
          <slur type="start" number="4"/>
          <articulations><caesura/></articulations>
          <ornaments><mordent/><tremolo type="single">2</tremolo><wavy-line type="stop"/></ornaments>
          <technical><fingering>3</fingering></technical>
          <arpeggiate direction="down"/>
          <glissando type="stop"/>
        </notations>
      </note>
      <direction placement="below"><direction-type><octave-shift type="up" size="15"/></direction-type></direction>
      <direction placement="below"><direction-type><pedal type="start" soft="yes"/></direction-type></direction>
      <direction placement="below"><direction-type><wedge type="diminuendo" number="3"/></direction-type></direction>
      <note>
        <pitch><step>E</step><octave>5</octave></pitch>
        <duration>4</duration><voice>1</voice><type>quarter</type>
        <notations>
          <ornaments><turn/><tremolo type="single">3</tremolo></ornaments>
          <technical><fingering>5</fingering></technical>
          <non-arpeggiate type="bottom"/>
        </notations>
      </note>
      <direction placement="below"><direction-type><octave-shift type="stop" size="15"/></direction-type></direction>
      <direction placement="below"><direction-type><pedal type="stop" soft="yes"/></direction-type></direction>
      <direction placement="below"><direction-type><wedge type="stop" number="3"/></direction-type></direction>
      <note>
        <pitch><step>F</step><octave>5</octave></pitch>
        <duration>4</duration><voice>1</voice><type>quarter</type>
        <notations>
          <slur type="stop" number="4"/>
          <ornaments><inverted-turn/><tremolo type="start">3</tremolo></ornaments>
        </notations>
      </note>
    </measure>
  </part>
</score-partwise>"#;

#[test]
fn bekern_full_layer2_lossless_expressive_split_roundtrip() {
    let headers = staff_headers(LAYER2_XML).unwrap();
    let events = bekern_events(LAYER2_XML).unwrap();
    let (toks, _) = to_bekern_full(&events, &headers).unwrap();
    let s = toks.join(" ");

    for expected in [
        "marcato",
        "mordent-up",
        "mordent-lo",
        "turn",
        "turn-inv",
        "tremolo-1",
        "tremolo-2",
        "tremolo-3",
        "tremolo-2n",
        "15ma-open",
        "15ma-close",
        "15mb-open",
        "15mb-close",
        "pedal-sost-open",
        "pedal-sost-close",
        "pedal-uc-open",
        "pedal-uc-close",
        "arp-up",
        "arp-down",
        "arp-non",
        "trill-line-open",
        "trill-line-close",
        "gliss-open",
        "gliss-close",
        "slur-2-open",
        "slur-2-close",
        "slur-4-open",
        "slur-4-close",
        "hairpin-2-cresc",
        "hairpin-2-close",
        "hairpin-3-dim",
        "hairpin-3-close",
        "breath",
        "caesura",
        "fing-1",
        "fing-3",
        "fing-5",
        "harmonics",
    ] {
        assert!(
            s.contains(expected),
            "missing Layer 2 token {expected:?} in: {s}"
        );
    }

    let input = decode_bekern_unmerged(&s).expect("decode Layer 2 full stream");
    let (score, mut losses) = parse_score(&input).expect("parse Layer 2 score");
    let xml = to_musicxml(&score, &mut losses);
    assert!(xml.contains("<strong-accent/>"));
    assert!(xml.contains("<inverted-mordent/>"));
    assert!(xml.contains("<mordent/>"));
    assert!(xml.contains("<turn/>"));
    assert!(xml.contains("<inverted-turn/>"));
    assert!(xml.contains("<tremolo type=\"single\">1</tremolo>"));
    assert!(xml.contains("<tremolo type=\"single\">2</tremolo>"));
    assert!(xml.contains("<tremolo type=\"single\">3</tremolo>"));
    assert!(xml.contains("<tremolo type=\"start\">3</tremolo>"));
    assert!(xml.contains("<octave-shift type=\"down\" size=\"15\"/>"));
    assert!(xml.contains("<octave-shift type=\"up\" size=\"15\"/>"));
    assert!(xml.contains("<pedal type=\"sostenuto\"/>"));
    assert!(xml.contains("<pedal type=\"start\"/>"));
    assert!(!xml.contains("sostenuto=\"yes\""));
    assert!(!xml.contains("soft=\"yes\""));
    assert!(xml.contains("<arpeggiate direction=\"up\"/>"));
    assert!(xml.contains("<arpeggiate direction=\"down\"/>"));
    assert!(xml.contains("<non-arpeggiate"));
    assert!(xml.contains("<wavy-line type=\"start\"/>"));
    assert!(xml.contains("<wavy-line type=\"stop\"/>"));
    assert!(xml.contains("<glissando type=\"start\""));
    assert!(xml.contains("<glissando type=\"stop\"/>"));
    assert!(xml.contains("<slur type=\"start\" number=\"2\"/>"));
    assert!(xml.contains("<slur type=\"stop\" number=\"4\"/>"));
    assert!(xml.contains("<wedge type=\"crescendo\" number=\"2\"/>"));
    assert!(xml.contains("<wedge type=\"diminuendo\" number=\"3\"/>"));
    assert!(xml.contains("<breath-mark/>"));
    assert!(xml.contains("<caesura/>"));
    assert!(xml.contains("<fingering>1</fingering>"));
    assert!(xml.contains("<harmonic/>"));
    let issues = validate_musicxml(&xml);
    assert!(!has_errors(&issues), "Layer 2 MusicXML errors: {issues:?}");
}

const LAYER3_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<score-partwise version="3.1">
  <part-list>
    <score-part id="P1"><part-name>Vocal</part-name></score-part>
  </part-list>
  <part id="P1">
    <measure number="1">
      <attributes>
        <divisions>4</divisions>
        <key><fifths>0</fifths></key>
        <time><beats>4</beats><beat-type>4</beat-type></time>
        <clef><sign>G</sign><line>2</line></clef>
      </attributes>
      <direction placement="above"><direction-type><rehearsal>A</rehearsal></direction-type></direction>
      <direction placement="above"><direction-type><words>Allegro ma non troppo</words></direction-type><sound tempo="120"/></direction>
      <harmony><root><root-step>C</root-step></root><kind text="Cmaj7">major-seventh</kind></harmony>
      <figured-bass><figure><figure-number>6</figure-number></figure><figure><figure-number>4</figure-number></figure></figured-bass>
      <note>
        <pitch><step>C</step><octave>4</octave></pitch>
        <duration>4</duration><voice>1</voice><type>quarter</type>
        <lyric number="1"><syllabic>begin</syllabic><text>Früh</text></lyric>
        <lyric number="2"><syllabic>single</syllabic><text>Spring</text><extend/></lyric>
      </note>
      <direction placement="below"><direction-type><words>dolce espressivo</words></direction-type></direction>
      <note>
        <pitch><step>D</step><octave>4</octave></pitch>
        <duration>4</duration><voice>1</voice><type>quarter</type>
        <lyric number="1"><syllabic>middle</syllabic><text>lings</text></lyric>
      </note>
      <note>
        <pitch><step>E</step><octave>4</octave></pitch>
        <duration>4</duration><voice>1</voice><type>quarter</type>
        <lyric number="1"><syllabic>end</syllabic><text>lied</text></lyric>
      </note>
      <note>
        <pitch><step>G</step><octave>4</octave></pitch>
        <duration>4</duration><voice>1</voice><type>quarter</type>
        <lyric number="1"><syllabic>single</syllabic><text>ah</text><extend/></lyric>
      </note>
    </measure>
  </part>
</score-partwise>"#;

#[test]
fn bekern_full_layer3_anchored_lyrics_and_text_spans_roundtrip() {
    let headers = staff_headers(LAYER3_XML).unwrap();
    let events = bekern_events(LAYER3_XML).unwrap();
    let (toks, _) = to_bekern_full(&events, &headers).unwrap();
    let s = toks.join(" ");

    // Lyrics & text spans must appear AFTER the final `bar` token (end-of-system).
    let bar_pos = s.find(" bar ").expect("bar token before end-of-system text");
    let tail = &s[bar_pos..];
    for expected in [
        "<sys_lyr> v=1",
        "@0 lyr-beg Früh",
        "@1 lyr-mid lings",
        "@2 lyr-end lied",
        "@3 lyr-ext ah",
        "<sys_lyr> v=2",
        "@0 lyr-ext Spring",
        "<reh> @0 A </text_span>",
        "<tempo> @0 Allegro ma non troppo </text_span>",
        "<harm> @0 Cmaj7 </text_span>",
        "<fb> @0 6/4 </text_span>",
        "<dir> @1 dolce espressivo </text_span>",
    ] {
        assert!(
            tail.contains(expected),
            "missing Layer 3 end-of-system span {expected:?} in tail: {tail}"
        );
    }

    let input = decode_bekern_unmerged(&s).expect("decode Layer 3 full stream");
    assert_eq!(input.parts[0].notes[0].lyric.as_deref(), Some("Früh"));
    let (score, mut losses) = parse_score(&input).expect("parse Layer 3 score");
    let xml = to_musicxml(&score, &mut losses);
    assert!(xml.contains("<lyric number=\"1\"><syllabic>begin</syllabic><text>Früh</text></lyric>"));
    assert!(xml.contains("<lyric number=\"2\"><syllabic>single</syllabic><text>Spring</text><extend/></lyric>"));
    assert!(xml.contains("<lyric number=\"1\"><syllabic>middle</syllabic><text>lings</text></lyric>"));
    assert!(xml.contains("<lyric number=\"1\"><syllabic>end</syllabic><text>lied</text></lyric>"));
    assert!(xml.contains("<rehearsal>A</rehearsal>"));
    assert!(xml.contains("<words>Allegro ma non troppo</words>"));
    assert!(xml.contains("<words>dolce espressivo</words>"));
    assert!(xml.contains("text=\"Cmaj7\""));
    assert!(xml.contains("<figure><figure-number>6</figure-number></figure><figure><figure-number>4</figure-number></figure>"));
    let issues = validate_musicxml(&xml);
    assert!(!has_errors(&issues), "Layer 3 MusicXML errors: {issues:?}");
}

#[test]
fn bekern_v1_and_v2_strict_backward_compatibility() {
    for xml in [LAYER0_XML, LAYER1_XML, LAYER2_XML, LAYER3_XML] {
        let headers = staff_headers(xml).unwrap();
        let events = bekern_events(xml).unwrap();

        let (v1_a, g1_a) = to_bekern(&events, &headers).unwrap();
        let (v1_b, g1_b) = to_bekern_with_dialect(&events, &headers, BekernDialect::V1).unwrap();
        assert_eq!(v1_a, v1_b);
        assert_eq!(g1_a, g1_b);

        let (v2_a, g2_a) = to_bekern_v2(&events, &headers).unwrap();
        let (v2_b, g2_b) = to_bekern_with_dialect(&events, &headers, BekernDialect::V2).unwrap();
        assert_eq!(v2_a, v2_b);
        assert_eq!(g2_a, g2_b);

        for (label, stream) in [("v1", v1_a.join(" ")), ("v2", v2_a.join(" "))] {
            for forbidden in [
                "<schema:full>",
                "<part>",
                "<staff-def>",
                "<group>",
                "<transpose>",
                "<pickup>",
                "<multirest>",
                "<chord>",
                "tup-open-11:8",
                "marcato",
                "mordent-up",
                "15ma-open",
                "<sys_lyr",
                "</text_span>",
            ] {
                assert!(
                    !stream.contains(forbidden),
                    "{label} leaked bekern-full token {forbidden:?}: {stream}"
                );
            }
        }
    }
}

#[test]
fn regression_r1_slur_open_close_no_panic() {
    let stream = "<schema:full> clef-G-2 key0 time-4-4 voice-1 quarter C 4 slur-open quarter D 4 slur-close bar";
    // Strict mode returns Err rather than panicking on OOB slice.
    assert!(decode_bekern_unmerged(stream).is_err());
    // Lenient mode recovers cleanly without panicking.
    let (input, warnings) = decode_bekern_lenient(stream).expect("lenient decode");
    assert_eq!(input.parts[0].notes.len(), 2);
    assert!(warnings.skipped_tokens >= 2);
}

#[test]
fn regression_r1_numbered_hairpin_single_wedge() {
    let stream = "<schema:full> clef-G-2 key0 time-4-4 hairpin-2-cresc voice-1 quarter C 4 hairpin-2-close quarter D 4 bar";
    let input = decode_bekern_unmerged(stream).expect("decode numbered hairpin");
    let (score, mut losses) = parse_score(&input).expect("parse score");
    let xml = to_musicxml(&score, &mut losses);
    assert_eq!(
        xml.matches("<wedge ").count(),
        2,
        "expected exactly 2 <wedge> elements (cresc + stop), got:\n{xml}"
    );
    assert!(xml.contains("<wedge type=\"crescendo\" number=\"2\"/>"));
    assert!(xml.contains("<wedge type=\"stop\" number=\"2\"/>"));
}

#[test]
fn regression_r1_at_index_alignment_with_grace_and_merged_ties() {
    // System notes in encoder order:
    //   @0 = grace B4 (dropped by merge_ties=true)
    //   @1 = C4 tie-start (swallowed by merge_ties=true)
    //   @2 = C4 tie-stop (emitted as merged C4)
    //   @3 = D4 (target of lyric "world")
    let stream = "<schema:full> clef-G-2 key0 time-4-4 voice-1 grace eighth B 4 quarter C 4 tie-start quarter C 4 tie-stop quarter D 4 bar <sys_lyr> v=1 @3 world </text_span>";
    let input = decode_bekern(stream).expect("decode with merge_ties=true");
    let notes = &input.parts[0].notes;
    assert_eq!(notes.len(), 2, "expected merged C4 + D4");
    assert_eq!(notes[0].pitch.as_deref(), Some("C4"));
    assert_eq!(notes[0].lyric, None);
    assert_eq!(notes[1].pitch.as_deref(), Some("D4"));
    assert_eq!(notes[1].lyric.as_deref(), Some("world"));
}

#[test]
fn regression_r1_to_coda_stays_direction_and_xml_entities_roundtrip() {
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<score-partwise version="3.1">
  <part-list><score-part id="P1"><part-name>Strings &amp; Winds</part-name></score-part></part-list>
  <part id="P1">
    <measure number="1">
      <attributes>
        <divisions>4</divisions><key><fifths>0</fifths></key>
        <time><beats>4</beats><beat-type>4</beat-type></time>
        <clef><sign>G</sign><line>2</line></clef>
      </attributes>
      <direction placement="above"><direction-type><words>To Coda</words></direction-type></direction>
      <direction placement="below"><direction-type><words>dolce &amp; espressivo</words></direction-type></direction>
      <note>
        <pitch><step>C</step><octave>4</octave></pitch>
        <duration>16</duration><voice>1</voice><type>whole</type>
        <lyric number="1"><syllabic>single</syllabic><text>A &amp; B</text></lyric>
      </note>
    </measure>
  </part>
</score-partwise>"#;
    let headers = staff_headers(xml).unwrap();
    let events = bekern_events(xml).unwrap();
    let (toks, _) = to_bekern_full(&events, &headers).unwrap();
    let s = toks.join(" ");
    assert!(!s.contains("<inst-change>"), "To Coda must not be <inst-change>: {s}");
    assert!(s.contains("<dir> @0 To Coda </text_span>"), "missing To Coda dir: {s}");
    assert!(s.contains("<dir> @0 dolce & espressivo </text_span>"), "unescaped & dir: {s}");
    assert!(s.contains("<sys_lyr> v=1 @0 A & B </text_span>"), "plain single lyric without lyr-end: {s}");

    let input = decode_bekern_unmerged(&s).expect("decode entity stream");
    let (score, mut losses) = parse_score(&input).expect("parse entity score");
    let out_xml = to_musicxml(&score, &mut losses);
    assert!(out_xml.contains("<words>To Coda</words>"));
    assert!(out_xml.contains("<words>dolce &amp; espressivo</words>"));
    assert!(out_xml.contains("<text>A &amp; B</text>"));
    assert!(!out_xml.contains("&amp;amp;"), "double-escaped entity in:\n{out_xml}");
}

#[test]
fn regression_r1_part_group_stop_and_distinct_part_index() {
    // Two distinct parts (G2 Violin + F4 Cello) with explicit <part> markers must NOT
    // merge into a single piano grand staff, and <part-group> must emit matching stop.
    let stream = "<schema:full> <group> bracket <part> <staff-def> lines-5 clef-G-2 key0 time-4-4 <part> <staff-def> lines-5 clef-F-4 key0 time-4-4 voice-1 whole C 5 <staff> voice-1 whole C 3 bar";
    let input = decode_bekern_unmerged(stream).expect("decode two distinct parts");
    let (score, mut losses) = parse_score(&input).expect("parse two parts");
    let xml = to_musicxml(&score, &mut losses);
    assert!(xml.contains("<part-group type=\"start\" number=\"1\"><group-symbol>bracket</group-symbol></part-group>"));
    assert!(xml.contains("<part-group type=\"stop\" number=\"1\"/>"));
    assert!(xml.contains("<part id=\"P1\">"));
    assert!(xml.contains("<part id=\"P2\">"));
    assert!(!xml.contains("<staves>2</staves>"), "distinct parts must not merge into grand staff");
    let issues = validate_musicxml(&xml);
    assert!(!has_errors(&issues), "MusicXML errors: {issues:?}");
}

#[test]
fn regression_r1_15ma_15mb_close_tremolo_toggle_pickup_and_dual_accent() {
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<score-partwise version="3.1">
  <part-list><score-part id="P1"><part-name>Piano</part-name></score-part></part-list>
  <part id="P1">
    <measure number="0" implicit="yes">
      <attributes>
        <divisions>4</divisions><key><fifths>0</fifths></key>
        <time><beats>4</beats><beat-type>4</beat-type></time>
        <clef><sign>G</sign><line>2</line></clef>
      </attributes>
      <note>
        <pitch><step>G</step><octave>4</octave></pitch>
        <duration>4</duration><voice>1</voice><type>quarter</type>
        <notations><articulations><accent/><strong-accent/></articulations></notations>
      </note>
    </measure>
    <measure number="1">
      <direction placement="above"><direction-type><octave-shift type="down" size="15"/></direction-type></direction>
      <note>
        <pitch><step>C</step><octave>5</octave></pitch>
        <duration>8</duration><voice>1</voice><type>half</type>
        <notations><ornaments><tremolo type="start">3</tremolo></ornaments></notations>
      </note>
      <direction placement="above"><direction-type><octave-shift type="stop" size="15"/></direction-type></direction>
      <note>
        <pitch><step>E</step><octave>5</octave></pitch>
        <duration>8</duration><voice>1</voice><type>half</type>
        <notations><ornaments><tremolo type="stop">3</tremolo></ornaments></notations>
      </note>
    </measure>
  </part>
</score-partwise>"#;
    let headers = staff_headers(xml).unwrap();
    let events = bekern_events(xml).unwrap();
    let (toks, _) = to_bekern_full(&events, &headers).unwrap();
    let s = toks.join(" ");
    assert!(s.contains("15ma-open"));
    assert!(s.contains("15ma-close"));
    assert!(s.contains("accent") && s.contains("marcato"), "both accent and marcato must be preserved: {s}");

    let input = decode_bekern_unmerged(&s).expect("decode stream");
    let (score, mut losses) = parse_score(&input).expect("parse score");
    let out_xml = to_musicxml(&score, &mut losses);
    assert!(out_xml.contains("<tremolo type=\"start\">3</tremolo>"));
    assert!(out_xml.contains("<tremolo type=\"stop\">3</tremolo>"));
    assert!(out_xml.contains("<accent/>") && out_xml.contains("<strong-accent/>"));
    // Pickup measure (measure 0) must NOT be padded with a trailing rest.
    let m0_end = out_xml.find("</measure>").expect("first measure end");
    assert!(
        !out_xml[..m0_end].contains("<rest/>"),
        "pickup measure must not have trailing rest padding:\n{}",
        &out_xml[..m0_end]
    );
    let issues = validate_musicxml(&out_xml);
    assert!(!has_errors(&issues), "MusicXML errors: {issues:?}");
}

#[test]
fn regression_r2_lines_zero_rejected_and_15_stop_compat() {
    // 1. `<staff-def> lines-0` must be rejected (strict decode fails on unconsumed `lines-0`).
    let bad_lines = "<staff-def> lines-0 clef-G-2 key0 time-4-4 quarter C 4";
    let err = decode_bekern_unmerged(bad_lines)
        .expect_err("`lines-0` must not be accepted as a valid staff line count");
    assert!(
        err.contains("lines-0"),
        "expected error on `lines-0`, got: {err}"
    );

    // 2. `"15-stop"` compat alias decodes identically to `"15ma-close"` / `"15mb-close"`.
    let compat_stream = "clef-G-2 key0 time-4-4 15ma-open half C 5 15-stop half E 5";
    let input = decode_bekern_unmerged(compat_stream).expect("decode 15-stop compat stream");
    let (score, mut losses) = parse_score(&input).expect("parse score");
    let out_xml = to_musicxml(&score, &mut losses);
    assert!(
        out_xml.contains("<octave-shift type=\"down\" size=\"15\"/>"),
        "expected 15ma open in MusicXML:\n{out_xml}"
    );
    assert!(
        out_xml.contains("<octave-shift type=\"stop\" size=\"15\"/>"),
        "expected 15-stop to emit size=15 stop in MusicXML:\n{out_xml}"
    );
    let issues = validate_musicxml(&out_xml);
    assert!(!has_errors(&issues), "MusicXML errors: {issues:?}");
}
