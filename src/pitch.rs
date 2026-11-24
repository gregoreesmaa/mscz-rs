//! Music-theory pitch parsing, MIDI conversion, key-aware enharmonic spelling,
//! and MuseScore tonal pitch class (TPC) helpers.
//!
//! All functions in this module are pure, allocation-free (except error strings),
//! and safe to use on `wasm32-unknown-unknown` targets.

/// A parsed scientific pitch such as `"C4"`, `"F#5"`, `"Bb3"`, `"C##4"`, or `"A4+30c"`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParsedPitch {
    /// Diatonic step letter (`'A'`..=`'G'`).
    pub step: char,
    /// Chromatic alteration in semitones (`-2` = double flat, `-1` = flat,
    /// `0` = natural, `+1` = sharp, `+2` = double sharp).
    pub alter: i32,
    /// Scientific pitch octave (`-1..=9`, where middle C is `C4`).
    pub octave: i32,
    /// Microtonal offset in cents (`-100.0 < cents < 100.0`, `0.0` when absent).
    pub cents: f64,
}

fn step_semitone(step: char) -> i32 {
    match step.to_ascii_uppercase() {
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

/// Parse a scientific pitch string into [`ParsedPitch`].
///
/// Supported forms:
/// - Natural: `"C4"`, `"a3"`
/// - Sharps / flats / naturals: `"F#5"`, `"Bb3"`, `"Cn4"`
/// - Double sharps / double flats: `"C##4"`, `"Gx2"`, `"Bbb3"`
/// - Negative octaves: `"C-1"`, `"Eb-1"`
/// - Optional cents suffix: `"A4+30c"`, `"G3-12.5c"`, `"D#4+15"`
pub fn parse_pitch(s: &str) -> Result<ParsedPitch, String> {
    let t = s.trim();
    let mut it = t.chars();
    let step = it
        .next()
        .ok_or_else(|| format!("empty pitch {s:?}"))?
        .to_ascii_uppercase();
    if !"ABCDEFG".contains(step) {
        return Err(format!("bad step in pitch {s:?}"));
    }

    let mut rest = it.as_str();
    let mut alter = 0i32;
    let mut sharp_count = 0i32;
    let mut flat_count = 0i32;
    loop {
        if let Some(r) = rest.strip_prefix('#') {
            sharp_count += 1;
            alter += 1;
            rest = r;
        } else if let Some(r) = rest.strip_prefix('x').or_else(|| rest.strip_prefix('X')) {
            sharp_count += 2;
            alter += 2;
            rest = r;
        } else if let Some(r) = rest.strip_prefix('b').or_else(|| rest.strip_prefix('B')) {
            flat_count += 1;
            alter -= 1;
            rest = r;
        } else if let Some(r) = rest.strip_prefix('n').or_else(|| rest.strip_prefix('N')) {
            rest = r;
        } else {
            break;
        }
    }
    if sharp_count > 2 || alter > 2 {
        return Err(format!("triple sharp unsupported in pitch {s:?}"));
    }
    if flat_count > 2 || alter < -2 {
        return Err(format!("triple flat unsupported in pitch {s:?}"));
    }

    // Octave: optional '-', then ASCII digits.
    let mut dig = String::new();
    let mut tail = rest;
    if let Some(r) = tail.strip_prefix('-') {
        dig.push('-');
        tail = r;
    } else if let Some(r) = tail.strip_prefix('+') {
        tail = r;
    }
    while let Some(c) = tail.chars().next() {
        if c.is_ascii_digit() {
            dig.push(c);
            tail = &tail[1..];
        } else {
            break;
        }
    }
    if dig.is_empty() || dig == "-" {
        return Err(format!("missing octave in pitch {s:?}"));
    }
    let octave: i32 = dig
        .parse()
        .map_err(|_| format!("bad octave in pitch {s:?}"))?;
    if !(-1..=9).contains(&octave) {
        return Err(format!("octave out of range in pitch {s:?}"));
    }

    let base_midi = pitch_to_midi(step, alter, octave);
    let mut cents = 0.0f64;
    let tail_t = tail.trim();
    if !tail_t.is_empty() {
        let (num, ok) = match tail_t
            .strip_suffix('c')
            .or_else(|| tail_t.strip_suffix('C'))
        {
            Some(n) => (n, true),
            None => (tail_t, tail_t.starts_with('+') || tail_t.starts_with('-')),
        };
        let c: f64 = if ok {
            num.parse()
                .map_err(|_| format!("bad cents suffix in pitch {s:?}"))?
        } else {
            return Err(format!("trailing garbage in pitch {s:?}"));
        };
        if !c.is_finite() || c.abs() >= 100.0 {
            return Err(format!("cents out of range in pitch {s:?}"));
        }
        cents = c;
    }

    let rounded_midi = ((base_midi as f64) + cents / 100.0).round() as i32;
    if !(0..=127).contains(&rounded_midi) {
        return Err(format!(
            "pitch {s:?} out of MIDI range (got {rounded_midi})"
        ));
    }

    Ok(ParsedPitch {
        step,
        alter,
        octave,
        cents,
    })
}

/// Convert `(step, alter, octave)` to a MIDI note number (`C4` = `60`, `A4` = `69`, `C-1` = `0`).
pub fn pitch_to_midi(step: char, alter: i32, octave: i32) -> i32 {
    (octave + 1) * 12 + step_semitone(step) + alter
}

/// Convert a MIDI note number (`0..=127`) to `(step, alter, octave)`.
///
/// When `prefer_flats` is `false`, accidentals are spelled as sharps (`C#`, `D#`, `F#`, `G#`, `A#`).
/// When `prefer_flats` is `true`, accidentals are spelled as flats (`Db`, `Eb`, `Gb`, `Ab`, `Bb`).
pub fn midi_to_pitch(midi: u8, prefer_flats: bool) -> (char, i32, i32) {
    let m = midi as i32;
    let octave = m.div_euclid(12) - 1;
    let (step, alter) = match m.rem_euclid(12) {
        0 => ('C', 0),
        1 => {
            if prefer_flats {
                ('D', -1)
            } else {
                ('C', 1)
            }
        }
        2 => ('D', 0),
        3 => {
            if prefer_flats {
                ('E', -1)
            } else {
                ('D', 1)
            }
        }
        4 => ('E', 0),
        5 => ('F', 0),
        6 => {
            if prefer_flats {
                ('G', -1)
            } else {
                ('F', 1)
            }
        }
        7 => ('G', 0),
        8 => {
            if prefer_flats {
                ('A', -1)
            } else {
                ('G', 1)
            }
        }
        9 => ('A', 0),
        10 => {
            if prefer_flats {
                ('B', -1)
            } else {
                ('A', 1)
            }
        }
        _ => ('B', 0),
    };
    (step, alter, octave)
}

/// Convert a MIDI note number (`0..=127`) to `(step, alter, octave)` using
/// enharmonic spelling appropriate for `key_fifths` (`-7..=7`, where `0` = C major,
/// positive = sharps, negative = flats).
///
/// For extreme sharp/flat keys, diatonic enharmonics such as `E#` (`key_fifths >= 6`),
/// `B#` (`key_fifths >= 7`), `Cb` (`key_fifths <= -6`), and `Fb` (`key_fifths <= -7`)
/// are spelled with the matching octave adjustment so `pitch_to_midi(step, alter, octave) == midi as i32`.
pub fn midi_to_pitch_for_key(midi: u8, key_fifths: i32) -> (char, i32, i32) {
    let m = midi as i32;
    let octave = m.div_euclid(12) - 1;
    let pc = m.rem_euclid(12);

    if key_fifths >= 7 && pc == 0 {
        return ('B', 1, octave - 1);
    }
    if key_fifths >= 6 && pc == 5 {
        return ('E', 1, octave);
    }
    if key_fifths <= -6 && pc == 11 {
        return ('C', -1, octave + 1);
    }
    if key_fifths <= -7 && pc == 4 {
        return ('F', -1, octave);
    }

    midi_to_pitch(midi, key_fifths < 0)
}

/// MuseScore line-of-fifths Tonal Pitch Class (TPC), where `C = 14`, `G = 15`, `F = 13`.
///
/// Each sharp adds `7` and each flat subtracts `7` (e.g. `F# = 20`, `Bb = 12`, `C## = 28`).
pub fn tonal_pitch_class(step: char, alter: i32) -> i32 {
    let base = match step.to_ascii_uppercase() {
        'C' => 14,
        'D' => 16,
        'E' => 18,
        'F' => 13,
        'G' => 15,
        'A' => 17,
        'B' => 19,
        _ => 14,
    };
    base + alter * 7
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic_and_accidentals() {
        assert_eq!(
            parse_pitch("C4").unwrap(),
            ParsedPitch {
                step: 'C',
                alter: 0,
                octave: 4,
                cents: 0.0
            }
        );
        assert_eq!(
            parse_pitch("F#5").unwrap(),
            ParsedPitch {
                step: 'F',
                alter: 1,
                octave: 5,
                cents: 0.0
            }
        );
        assert_eq!(
            parse_pitch("Bb3").unwrap(),
            ParsedPitch {
                step: 'B',
                alter: -1,
                octave: 3,
                cents: 0.0
            }
        );
        assert_eq!(
            parse_pitch("C##4").unwrap(),
            ParsedPitch {
                step: 'C',
                alter: 2,
                octave: 4,
                cents: 0.0
            }
        );
        assert_eq!(
            parse_pitch("Gx2").unwrap(),
            ParsedPitch {
                step: 'G',
                alter: 2,
                octave: 2,
                cents: 0.0
            }
        );
        assert_eq!(
            parse_pitch("Bbb3").unwrap(),
            ParsedPitch {
                step: 'B',
                alter: -2,
                octave: 3,
                cents: 0.0
            }
        );
        assert_eq!(
            parse_pitch("eb-1").unwrap(),
            ParsedPitch {
                step: 'E',
                alter: -1,
                octave: -1,
                cents: 0.0
            }
        );
    }

    #[test]
    fn parse_cents_suffix() {
        let p = parse_pitch("A4+30c").unwrap();
        assert_eq!(p.step, 'A');
        assert_eq!(p.alter, 0);
        assert_eq!(p.octave, 4);
        assert!((p.cents - 30.0).abs() < 1e-9);

        let p2 = parse_pitch("G3-12.5c").unwrap();
        assert_eq!(p2.step, 'G');
        assert!((p2.cents - (-12.5)).abs() < 1e-9);
    }

    #[test]
    fn parse_rejects_invalid() {
        assert!(parse_pitch("").is_err());
        assert!(parse_pitch("H4").is_err());
        assert!(parse_pitch("C").is_err());
        assert!(parse_pitch("C###4").is_err());
        assert!(parse_pitch("Bbbb4").is_err());
        assert!(parse_pitch("C10").is_err());
        assert!(parse_pitch("A4+100c").is_err());
        assert!(parse_pitch("C-1b").is_err());
    }

    #[test]
    fn pitch_midi_roundtrip() {
        assert_eq!(pitch_to_midi('C', 0, 4), 60);
        assert_eq!(pitch_to_midi('A', 0, 4), 69);
        assert_eq!(pitch_to_midi('C', 0, -1), 0);
        assert_eq!(pitch_to_midi('G', 0, 9), 127);

        for midi in 0u8..=127 {
            let (s, a, o) = midi_to_pitch(midi, false);
            assert_eq!(pitch_to_midi(s, a, o), midi as i32);
            let (sf, af, of) = midi_to_pitch(midi, true);
            assert_eq!(pitch_to_midi(sf, af, of), midi as i32);
        }
    }

    #[test]
    fn key_aware_spelling() {
        // F major (-1): MIDI 70 -> Bb4
        assert_eq!(midi_to_pitch_for_key(70, -1), ('B', -1, 4));
        // G major (+1): MIDI 66 -> F#4
        assert_eq!(midi_to_pitch_for_key(66, 1), ('F', 1, 4));
        // F# major (+6): MIDI 65 -> E#4
        assert_eq!(midi_to_pitch_for_key(65, 6), ('E', 1, 4));
        // C# major (+7): MIDI 60 -> B#3
        assert_eq!(midi_to_pitch_for_key(60, 7), ('B', 1, 3));
        // Gb major (-6): MIDI 59 -> Cb4
        assert_eq!(midi_to_pitch_for_key(59, -6), ('C', -1, 4));
        // Cb major (-7): MIDI 64 -> Fb4
        assert_eq!(midi_to_pitch_for_key(64, -7), ('F', -1, 4));

        for fifths in -7..=7 {
            for midi in 0u8..=127 {
                let (s, a, o) = midi_to_pitch_for_key(midi, fifths);
                assert_eq!(
                    pitch_to_midi(s, a, o),
                    midi as i32,
                    "midi={midi}, fifths={fifths} -> ({s}, {a}, {o})"
                );
            }
        }
    }

    #[test]
    fn tpc_line_of_fifths() {
        assert_eq!(tonal_pitch_class('C', 0), 14);
        assert_eq!(tonal_pitch_class('G', 0), 15);
        assert_eq!(tonal_pitch_class('F', 0), 13);
        assert_eq!(tonal_pitch_class('F', 1), 20);
        assert_eq!(tonal_pitch_class('B', -1), 12);
        assert_eq!(tonal_pitch_class('C', 2), 28);
        assert_eq!(tonal_pitch_class('d', -1), 9);
    }
}
