//! WebAssembly FFI entrypoints for client-side `bekern+` -> MusicXML / `.mscz` compilation.
//!
//! Designed to compile cleanly to `wasm32-unknown-unknown` as a `cdylib` with
//! zero external JS/WASM toolchains (no `wasm-bindgen` required):
//!
//! ```bash
//! cargo rustc --release --target wasm32-unknown-unknown \
//!     --no-default-features --features wasm --crate-type cdylib
//! ```
//!
//! Callers allocate UTF-8 input buffers via [`wasm_alloc`], invoke
//! [`bekern_to_mscz`] or [`bekern_to_musicxml`], and read the resulting bytes
//! from [`wasm_result_ptr`] / [`wasm_result_len`] (or error text from
//! [`wasm_error_ptr`] / [`wasm_error_len`] when the returned length is `0`).

use std::cell::RefCell;
use std::slice;

use crate::{decode, pack_mscz, parse_score, to_mscx, to_musicxml};

thread_local! {
    static LAST_RESULT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    static LAST_ERROR: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

fn is_header_token(tok: &str) -> bool {
    tok.starts_with("clef-") || tok.starts_with("key") || tok.starts_with("time-")
}

fn is_bar_token(tok: &str) -> bool {
    tok == "bar" || tok.starts_with("bar-")
}

/// Stitch a UniOMR full-page `bekern+` token stream (which may contain `<sys>`
/// system separators and `<bos>`/`<eos>`/`<pad>` special tokens) into a single
/// clean `bekern+` stream suitable for [`decode::decode_bekern_unmerged`].
///
/// - Drops `<bos>`, `<eos>`, `<pad>`.
/// - Splits on `<sys>` into per-system chunks.
/// - Deduplicates unchanged leading courtesy headers (`clef-*`, `key*`, `time-*`)
///   on subsequent systems while preserving genuine mid-page changes.
/// - Inserts a `bar` separator between adjacent systems if the preceding system
///   did not end with a barline token.
pub fn stitch_uni_bekern(raw: &str) -> String {
    let mut systems: Vec<Vec<&str>> = Vec::new();
    let mut cur: Vec<&str> = Vec::new();

    for tok in raw.split_whitespace() {
        match tok {
            "<bos>" | "<pad>" => continue,
            "<eos>" => break,
            "<sys>" => {
                if !cur.is_empty() {
                    systems.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(tok),
        }
    }
    if !cur.is_empty() {
        systems.push(cur);
    }

    if systems.is_empty() {
        return String::new();
    }
    if systems.len() == 1 {
        return systems[0].join(" ");
    }

    let mut out: Vec<&str> = Vec::new();
    let mut active_clefs: Vec<&str> = Vec::new();
    let mut active_key: Option<&str> = None;
    let mut active_time: Option<&str> = None;

    for (sys_idx, sys) in systems.iter().enumerate() {
        let mut idx = 0usize;
        if sys_idx == 0 {
            while idx < sys.len() && is_header_token(sys[idx]) {
                let tok = sys[idx];
                if tok.starts_with("clef-") {
                    active_clefs.push(tok);
                } else if tok.starts_with("key") {
                    active_key = Some(tok);
                } else if tok.starts_with("time-") {
                    active_time = Some(tok);
                }
                out.push(tok);
                idx += 1;
            }
        } else {
            if out.last().is_some_and(|t| !is_bar_token(t)) {
                out.push("bar");
            }
            // Strip unchanged courtesy headers at the start of system 2+,
            // supporting multi-staff clef sequences while still emitting
            // genuine mid-page clef/key/time changes.
            let mut clef_pos = 0usize;
            while idx < sys.len() && is_header_token(sys[idx]) {
                let tok = sys[idx];
                let unchanged = if tok.starts_with("clef-") {
                    let same = active_clefs.get(clef_pos).copied() == Some(tok);
                    if clef_pos < active_clefs.len() {
                        active_clefs[clef_pos] = tok;
                    } else {
                        active_clefs.push(tok);
                    }
                    clef_pos += 1;
                    same
                } else if tok.starts_with("key") {
                    let same = active_key == Some(tok);
                    active_key = Some(tok);
                    same
                } else if tok.starts_with("time-") {
                    let same = active_time == Some(tok);
                    active_time = Some(tok);
                    same
                } else {
                    false
                };
                if !unchanged {
                    out.push(tok);
                }
                idx += 1;
            }
        }

        // Emit remaining tokens of this system, tracking mid-system header changes.
        while idx < sys.len() {
            let tok = sys[idx];
            if tok.starts_with("clef-") && active_clefs.len() <= 1 {
                if active_clefs.is_empty() {
                    active_clefs.push(tok);
                } else {
                    active_clefs[0] = tok;
                }
            } else if tok.starts_with("key") {
                active_key = Some(tok);
            } else if tok.starts_with("time-") {
                active_time = Some(tok);
            }
            out.push(tok);
            idx += 1;
        }
    }

    out.join(" ")
}

/// Compile a `bekern+` token string (single-system or `<sys>`-delimited
/// full-page stream) directly into both native MuseScore 4 `.mscz` archive
/// bytes and a MusicXML 3.1 document string while parsing [`Score`] only once.
pub fn compile_bekern_to_mscz_and_musicxml(
    bekern_text: &str,
    title: Option<&str>,
) -> Result<(Vec<u8>, String), String> {
    let cleaned = stitch_uni_bekern(bekern_text);
    if cleaned.is_empty() {
        return Err("empty bekern token stream".to_string());
    }
    let mut decoded = decode::decode_bekern_unmerged(&cleaned)?;
    if let Some(t) = title.map(str::trim).filter(|t| !t.is_empty()) {
        decoded.title = Some(t.to_string());
    }
    let (score, mut losses) = parse_score(&decoded)?;
    let mscx = to_mscx(&score, &mut losses);
    let mscz = pack_mscz(&score, &mscx)?;
    let xml = to_musicxml(&score, &mut losses);
    Ok((mscz, xml))
}

/// Validate `bekern+` syntax by running `stitch_uni_bekern`,
/// `decode_bekern_unmerged`, and `parse_score` without XML or ZIP serialization.
pub fn validate_bekern_syntax_str(bekern_text: &str) -> Result<(), String> {
    let cleaned = stitch_uni_bekern(bekern_text);
    if cleaned.is_empty() {
        return Err("empty bekern token stream".to_string());
    }
    let decoded = decode::decode_bekern_unmerged(&cleaned)?;
    let _ = parse_score(&decoded)?;
    Ok(())
}

/// Compile a `bekern+` token string (single-system or `<sys>`-delimited
/// full-page stream) directly into native MuseScore 4 `.mscz` archive bytes.
pub fn compile_bekern_to_mscz(bekern_text: &str, title: Option<&str>) -> Result<Vec<u8>, String> {
    let cleaned = stitch_uni_bekern(bekern_text);
    if cleaned.is_empty() {
        return Err("empty bekern token stream".to_string());
    }
    let mut decoded = decode::decode_bekern_unmerged(&cleaned)?;
    if let Some(t) = title.map(str::trim).filter(|t| !t.is_empty()) {
        decoded.title = Some(t.to_string());
    }
    let (score, mut losses) = parse_score(&decoded)?;
    let mscx = to_mscx(&score, &mut losses);
    pack_mscz(&score, &mscx)
}

/// Compile a `bekern+` token string (single-system or `<sys>`-delimited
/// full-page stream) directly into a MusicXML 3.1 document string.
pub fn compile_bekern_to_musicxml(
    bekern_text: &str,
    title: Option<&str>,
) -> Result<String, String> {
    let cleaned = stitch_uni_bekern(bekern_text);
    if cleaned.is_empty() {
        return Err("empty bekern token stream".to_string());
    }
    let mut decoded = decode::decode_bekern_unmerged(&cleaned)?;
    if let Some(t) = title.map(str::trim).filter(|t| !t.is_empty()) {
        decoded.title = Some(t.to_string());
    }
    let (score, mut losses) = parse_score(&decoded)?;
    Ok(to_musicxml(&score, &mut losses))
}

fn set_result(bytes: Vec<u8>) -> usize {
    let len = bytes.len();
    LAST_ERROR.with(|e| e.borrow_mut().clear());
    LAST_RESULT.with(|r| *r.borrow_mut() = bytes);
    len
}

fn set_error(msg: String) -> usize {
    LAST_RESULT.with(|r| r.borrow_mut().clear());
    LAST_ERROR.with(|e| *e.borrow_mut() = msg.into_bytes());
    0
}

unsafe fn read_utf8_arg<'a>(ptr: *const u8, len: usize) -> Result<&'a str, String> {
    if len == 0 {
        return Ok("");
    }
    if ptr.is_null() {
        return Err("null input pointer".to_string());
    }
    let bytes = slice::from_raw_parts(ptr, len);
    std::str::from_utf8(bytes).map_err(|e| format!("invalid utf-8 input: {e}"))
}

/// Allocate `len` bytes in WASM linear memory for host-to-WASM string passing.
#[no_mangle]
pub extern "C" fn wasm_alloc(len: usize) -> *mut u8 {
    let mut buf = Vec::<u8>::with_capacity(len.max(1));
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr
}

/// Free a buffer previously allocated by [`wasm_alloc`].
///
/// # Safety
/// `ptr` must have been returned by `wasm_alloc(len)` and not already freed.
#[no_mangle]
pub unsafe extern "C" fn wasm_free(ptr: *mut u8, len: usize) {
    if !ptr.is_null() {
        let cap = len.max(1);
        drop(Vec::from_raw_parts(ptr, 0, cap));
    }
}

/// Convert UTF-8 `bekern+` text (and optional UTF-8 `title`) into `.mscz` bytes.
/// Returns the output byte length on success, or `0` on error (inspect
/// [`wasm_error_ptr`] / [`wasm_error_len`]).
///
/// # Safety
/// Input pointers must be valid for their respective byte lengths.
#[no_mangle]
pub unsafe extern "C" fn bekern_to_mscz(
    bekern_ptr: *const u8,
    bekern_len: usize,
    title_ptr: *const u8,
    title_len: usize,
) -> usize {
    let bekern = match read_utf8_arg(bekern_ptr, bekern_len) {
        Ok(s) => s,
        Err(e) => return set_error(e),
    };
    let title = match read_utf8_arg(title_ptr, title_len) {
        Ok(s) if !s.is_empty() => Some(s),
        Ok(_) => None,
        Err(e) => return set_error(e),
    };
    match compile_bekern_to_mscz(bekern, title) {
        Ok(bytes) => set_result(bytes),
        Err(e) => set_error(e),
    }
}

/// Convert UTF-8 `bekern+` text (and optional UTF-8 `title`) into MusicXML 3.1 UTF-8 bytes.
/// Returns the output byte length on success, or `0` on error.
///
/// # Safety
/// Input pointers must be valid for their respective byte lengths.
#[no_mangle]
pub unsafe extern "C" fn bekern_to_musicxml(
    bekern_ptr: *const u8,
    bekern_len: usize,
    title_ptr: *const u8,
    title_len: usize,
) -> usize {
    let bekern = match read_utf8_arg(bekern_ptr, bekern_len) {
        Ok(s) => s,
        Err(e) => return set_error(e),
    };
    let title = match read_utf8_arg(title_ptr, title_len) {
        Ok(s) if !s.is_empty() => Some(s),
        Ok(_) => None,
        Err(e) => return set_error(e),
    };
    match compile_bekern_to_musicxml(bekern, title) {
        Ok(xml) => set_result(xml.into_bytes()),
        Err(e) => set_error(e),
    }
}

/// Convert UTF-8 `bekern+` text (and optional UTF-8 `title`) into BOTH `.mscz`
/// and MusicXML 3.1 UTF-8 bytes in a single parse pass.
///
/// The result buffer is framed as:
/// `[u32_le mscz_len][mscz bytes][musicxml utf8 bytes]`.
/// Returns the total framed byte length on success, or `0` on error.
///
/// # Safety
/// Input pointers must be valid for their respective byte lengths.
#[no_mangle]
pub unsafe extern "C" fn bekern_to_mscz_and_musicxml(
    bekern_ptr: *const u8,
    bekern_len: usize,
    title_ptr: *const u8,
    title_len: usize,
) -> usize {
    let bekern = match read_utf8_arg(bekern_ptr, bekern_len) {
        Ok(s) => s,
        Err(e) => return set_error(e),
    };
    let title = match read_utf8_arg(title_ptr, title_len) {
        Ok(s) if !s.is_empty() => Some(s),
        Ok(_) => None,
        Err(e) => return set_error(e),
    };
    match compile_bekern_to_mscz_and_musicxml(bekern, title) {
        Ok((mscz, xml)) => {
            let mscz_len = match u32::try_from(mscz.len()) {
                Ok(n) => n,
                Err(_) => return set_error("mscz output exceeds u32 length".to_string()),
            };
            let xml_bytes = xml.into_bytes();
            let mut framed = Vec::with_capacity(4 + mscz.len() + xml_bytes.len());
            framed.extend_from_slice(&mscz_len.to_le_bytes());
            framed.extend_from_slice(&mscz);
            framed.extend_from_slice(&xml_bytes);
            set_result(framed)
        }
        Err(e) => set_error(e),
    }
}

/// Validate UTF-8 `bekern+` syntax without serializing `.mscx`/MusicXML/ZIP.
/// Returns `1` on success, or `0` on error (inspect [`wasm_error_ptr`] /
/// [`wasm_error_len`]).
///
/// # Safety
/// `bekern_ptr` must be valid for `bekern_len` bytes.
#[no_mangle]
pub unsafe extern "C" fn validate_bekern_syntax(
    bekern_ptr: *const u8,
    bekern_len: usize,
) -> u32 {
    let bekern = match read_utf8_arg(bekern_ptr, bekern_len) {
        Ok(s) => s,
        Err(e) => {
            set_error(e);
            return 0;
        }
    };
    match validate_bekern_syntax_str(bekern) {
        Ok(()) => {
            LAST_ERROR.with(|e| e.borrow_mut().clear());
            1
        }
        Err(e) => {
            set_error(e);
            0
        }
    }
}

/// Stitch newline-separated per-page `bekern+` streams into a single `<sys>`-joined
/// multi-page `bekern+` stream with carried-over headers.
/// Returns the output byte length on success, or `0` on error.
///
/// # Safety
/// `pages_ptr` must be valid for `pages_len` bytes.
#[no_mangle]
pub unsafe extern "C" fn stitch_pages_bekern(
    pages_ptr: *const u8,
    pages_len: usize,
) -> usize {
    let raw = match read_utf8_arg(pages_ptr, pages_len) {
        Ok(s) => s,
        Err(e) => return set_error(e),
    };
    let pages: Vec<&str> = raw.split('\n').collect();
    let stitched = crate::bekern::stitch_pages_bekern(&pages);
    if stitched.is_empty() {
        return set_error("empty bekern pages".to_string());
    }
    set_result(stitched.into_bytes())
}

/// Pointer to the most recent successful output buffer.
#[no_mangle]
pub extern "C" fn wasm_result_ptr() -> *const u8 {
    LAST_RESULT.with(|r| r.borrow().as_ptr())
}

/// Byte length of the most recent successful output buffer.
#[no_mangle]
pub extern "C" fn wasm_result_len() -> usize {
    LAST_RESULT.with(|r| r.borrow().len())
}

/// Pointer to the most recent UTF-8 error message.
#[no_mangle]
pub extern "C" fn wasm_error_ptr() -> *const u8 {
    LAST_ERROR.with(|e| e.borrow().as_ptr())
}

/// Byte length of the most recent UTF-8 error message.
#[no_mangle]
pub extern "C" fn wasm_error_len() -> usize {
    LAST_ERROR.with(|e| e.borrow().len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{has_errors, validate_mscz, validate_musicxml};

    #[test]
    fn wasm_single_system_bekern_to_mscz_and_musicxml() {
        let bekern = "clef-G-2 key-1 time-4-4 quarter C 4 quarter D 4 quarter E 4 quarter F 4 bar half G 4 half C 5 bar-light-heavy";
        let mscz_bytes = compile_bekern_to_mscz(bekern, Some("Browser Test")).unwrap();
        let issues = validate_mscz(&mscz_bytes);
        assert!(!has_errors(&issues), "mscz validation errors: {issues:?}");

        let xml = compile_bekern_to_musicxml(bekern, Some("Browser Test")).unwrap();
        let xml_issues = validate_musicxml(&xml);
        assert!(
            !has_errors(&xml_issues),
            "musicxml validation errors: {xml_issues:?}"
        );
        assert!(xml.contains("Browser Test"));
    }

    #[test]
    fn wasm_multisystem_sys_stitch_dedups_courtesy_headers() {
        let bekern = "<bos> clef-G-2 key+0 time-4-4 quarter C 4 quarter D 4 quarter E 4 quarter F 4 <sys> clef-G-2 key+0 time-4-4 half G 4 half A 4 bar-light-heavy <eos>";
        let stitched = stitch_uni_bekern(bekern);
        assert_eq!(
            stitched,
            "clef-G-2 key+0 time-4-4 quarter C 4 quarter D 4 quarter E 4 quarter F 4 bar half G 4 half A 4 bar-light-heavy"
        );
        let mscz_bytes = compile_bekern_to_mscz(bekern, None).unwrap();
        assert!(!has_errors(&validate_mscz(&mscz_bytes)));
    }

    #[test]
    fn wasm_empty_input_reports_error() {
        let err = compile_bekern_to_mscz("   <bos> <eos>  ", None).unwrap_err();
        assert!(err.contains("empty"));
    }

    #[test]
    fn wasm_stitch_pages_bekern_compiles_multipage_score() {
        let p1 = "<bos> clef-G-2 key-1 time-3-4 quarter F 4 half A 4 bar <eos>";
        let p2 = "<bos> clef-G-2 key-1 quarter C 5 half F 5 bar-light-heavy <eos>";
        let joined = format!("{p1}\n{p2}");
        let len = unsafe { stitch_pages_bekern(joined.as_ptr(), joined.len()) };
        assert!(len > 0);
        let stitched = unsafe {
            let slice = slice::from_raw_parts(wasm_result_ptr(), wasm_result_len());
            std::str::from_utf8(slice).unwrap().to_string()
        };
        assert_eq!(
            stitched,
            "clef-G-2 key-1 time-3-4 quarter F 4 half A 4 bar <sys> clef-G-2 key-1 time-3-4 quarter C 5 half F 5 bar-light-heavy"
        );
        let mscz_bytes = compile_bekern_to_mscz(&stitched, Some("Two-Page Waltz")).unwrap();
        assert!(!has_errors(&validate_mscz(&mscz_bytes)));
    }

    #[test]
    fn wasm_fused_bekern_to_mscz_and_musicxml_framing() {
        let bekern = "clef-G-2 key-1 time-4-4 quarter C 4 quarter D 4 quarter E 4 quarter F 4 bar half G 4 half C 5 bar-light-heavy";
        let title = "Fused Test";
        let total_len = unsafe {
            bekern_to_mscz_and_musicxml(bekern.as_ptr(), bekern.len(), title.as_ptr(), title.len())
        };
        assert!(total_len > 4);
        let framed = unsafe {
            slice::from_raw_parts(wasm_result_ptr(), wasm_result_len()).to_vec()
        };
        assert_eq!(framed.len(), total_len);
        let mscz_len = u32::from_le_bytes([framed[0], framed[1], framed[2], framed[3]]) as usize;
        assert!(mscz_len > 0 && 4 + mscz_len < framed.len());
        let mscz_bytes = &framed[4..4 + mscz_len];
        let xml = std::str::from_utf8(&framed[4 + mscz_len..]).unwrap();

        assert!(!has_errors(&validate_mscz(mscz_bytes)));
        assert!(!has_errors(&validate_musicxml(xml)));
        assert_eq!(mscz_bytes, compile_bekern_to_mscz(bekern, Some(title)).unwrap());
        assert_eq!(xml, compile_bekern_to_musicxml(bekern, Some(title)).unwrap());
    }

    #[test]
    fn wasm_validate_bekern_syntax_accepts_valid_and_rejects_malformed() {
        let valid = "clef-G-2 key+0 time-4-4 quarter C 4 half D 4 quarter E 4 bar-light-heavy";
        let ok = unsafe { validate_bekern_syntax(valid.as_ptr(), valid.len()) };
        assert_eq!(ok, 1);
        assert_eq!(wasm_error_len(), 0);

        let bad = "clef-G-2 key+0 time-4-4 F quarter C 4 bar";
        let err = unsafe { validate_bekern_syntax(bad.as_ptr(), bad.len()) };
        assert_eq!(err, 0);
        let msg = unsafe {
            let s = slice::from_raw_parts(wasm_error_ptr(), wasm_error_len());
            std::str::from_utf8(s).unwrap().to_string()
        };
        assert!(msg.contains("unexpected token"), "got: {msg}");
    }
}
