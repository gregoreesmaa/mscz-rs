//! Example 2: Convert a MusicXML 3.1 file into a validated MuseScore 4 `.mscz`
//! archive in pure Rust.
//!
//! Defaults to the vendored CC0 fixture `tests/fixtures/ds0001.musicxml` when
//! no CLI argument is provided.
//!
//! Run with:
//! ```bash
//! cargo run --example 02_musicxml_to_mscz
//! cargo run --example 02_musicxml_to_mscz -- path/to/score.musicxml [out.mscz]
//! ```

use std::env;
use std::fs;
use std::path::PathBuf;

use mscz::{
    has_errors, musicxml_import, pack_mscz, parse_score, score_pitched_count, to_mscx,
    validate_mscz,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().skip(1).collect();
    let input_path = if let Some(path) = args.first() {
        PathBuf::from(path)
    } else {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("ds0001.musicxml")
    };

    let xml = fs::read_to_string(&input_path)?;
    let score_input = musicxml_import::score_input_from_musicxml(&xml)?;
    let (score, mut losses) = parse_score(&score_input)?;
    let mscx = to_mscx(&score, &mut losses);
    let mscz_bytes = pack_mscz(&score, &mscx)?;

    let issues = validate_mscz(&mscz_bytes);
    if has_errors(&issues) {
        for issue in &issues {
            eprintln!("[{}] {}: {}", issue.severity, issue.code, issue.message);
        }
        return Err("generated .mscz failed validation".into());
    }

    if let Some(out_path) = args.get(1) {
        fs::write(out_path, &mscz_bytes)?;
        println!("Wrote:    {out_path}");
    }

    println!("Input:    {}", input_path.display());
    println!("Title:    {}", score.title);
    if let Some(composer) = &score.composer {
        println!("Composer: {composer}");
    }
    println!("Parts:    {}", score.parts.len());
    println!("Measures: {}", score.measure_count());
    println!("Notes:    {} pitched", score_pitched_count(&score));
    println!("MSCX:     {} bytes", mscx.len());
    println!(
        "MSCZ:     {} bytes (valid, {} issue(s))",
        mscz_bytes.len(),
        issues.len()
    );
    println!(
        "Losses:   {} loss(es) / {} total log entry/entries (lossless={})",
        losses.loss_count(),
        losses.entries.len(),
        losses.is_lossless()
    );

    Ok(())
}
