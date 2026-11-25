#!/usr/bin/env python3
"""External engine parity verifier for mscz-rs (MuseScore 4 + Verovio).

Runs real MusicXML scores through:
  1. `mscz bekern --page 1` -> canonical bekern+
  2. `mscz bekern2musicxml` -> `.musicxml` + `.mscz`
  3. Real MuseScore 4 (`-j` batch job with isolated TEMP/TMP shard, never
     setting QT_QPA_PLATFORM=offscreen) -> exports `.mscz` back to `.musicxml`
  4. Verovio (`verovio.toolkit()`) -> renders `.musicxml` to SVG

Compares staff count, opening clefs, and note-for-note (pitch, onset, dur)
F1 across GT MusicXML, mscz-rs MusicXML, and MuseScore 4 round-tripped MusicXML.
"""

from __future__ import annotations

import argparse
import collections
import json
import os
import shutil
import subprocess
import sys
import tempfile
import xml.etree.ElementTree as ET
from pathlib import Path

DEFAULT_PIECES = [
    "repeats-melody",       # 1-staff melody with repeats
    "bach-bwv66.6",         # 2-staff / 4-voice chorale
    "joplin-maple-leaf",    # 2-staff piano ragtime
    "chopin-mazurka06-2",   # 2-staff piano with dynamics/slurs
    "schubert-lindenbaum",  # 3-staff voice + piano
    "handel-lascia",        # 3-staff vocal + piano
    "mozart-k155-2",        # 4-staff string quartet
    "schumann-op41n1-3",    # 4-staff string quartet
]

STEP_TO_SEMITONE = {"C": 0, "D": 2, "E": 4, "F": 5, "G": 7, "A": 9, "B": 11}


def parse_musicxml_summary(xml_path: Path) -> dict:
    """Extract staff count, opening clefs, and merged pitched notes from MusicXML."""
    tree = ET.parse(xml_path)
    root = tree.getroot()

    clefs: list[str] = []
    total_staves = 0
    # Store pitched notes as (midi, measure_idx, onset_in_measure_ticks_480, dur_ticks_480)
    # after merging ties. Measure-relative onset aligns pickup measures cleanly with mscz-rs.
    notes: list[tuple[int, int, int, int]] = []

    for part_idx, part in enumerate(root.findall("part"), start=1):
        divisions = 1
        part_staves = 1
        part_clefs: dict[int, str] = {}
        seen_first_measure_attrs = False
        # Pending ties keyed by (staff, voice, midi) -> [measure_idx, onset_in_m_beats, dur_beats]
        pending_ties: dict[tuple[int, str, int], list[float]] = {}

        for m_idx, measure in enumerate(part.findall("measure"), start=1):
            cursor = 0.0  # beats within measure
            last_note_onset = 0.0

            for elem in measure:
                tag = elem.tag
                if tag == "attributes":
                    div_el = elem.find("divisions")
                    if div_el is not None and div_el.text:
                        divisions = max(1, int(float(div_el.text.strip())))
                    staves_el = elem.find("staves")
                    if staves_el is not None and staves_el.text:
                        part_staves = max(part_staves, int(staves_el.text.strip()))
                    if not seen_first_measure_attrs:
                        for clef_el in elem.findall("clef"):
                            num = int(clef_el.attrib.get("number", "1"))
                            sign = (clef_el.findtext("sign") or "G").strip()
                            line = (clef_el.findtext("line") or "2").strip()
                            oct_chg = clef_el.findtext("clef-octave-change")
                            suffix = ""
                            if oct_chg and oct_chg.strip() == "-1":
                                suffix = "-8vb"
                            elif oct_chg and oct_chg.strip() == "1":
                                suffix = "-8va"
                            if sign == "percussion":
                                sign = "p"
                            part_clefs.setdefault(num, f"{sign}-{line}{suffix}")
                elif tag == "backup":
                    dur_el = elem.find("duration")
                    if dur_el is not None and dur_el.text:
                        cursor -= float(dur_el.text.strip()) / divisions
                        if cursor < 0.0:
                            cursor = 0.0
                elif tag == "forward":
                    dur_el = elem.find("duration")
                    if dur_el is not None and dur_el.text:
                        cursor += float(dur_el.text.strip()) / divisions
                elif tag == "note":
                    if elem.find("grace") is not None:
                        continue
                    is_chord = elem.find("chord") is not None
                    dur_el = elem.find("duration")
                    dur_beats = (
                        float(dur_el.text.strip()) / divisions
                        if (dur_el is not None and dur_el.text)
                        else 0.0
                    )
                    if is_chord:
                        onset = last_note_onset
                    else:
                        onset = cursor
                        last_note_onset = cursor
                        cursor += dur_beats

                    if elem.find("rest") is not None:
                        continue
                    pitch_el = elem.find("pitch")
                    if pitch_el is None:
                        continue
                    step = (pitch_el.findtext("step") or "C").strip()
                    alter = int(round(float(pitch_el.findtext("alter") or "0")))
                    octave = int((pitch_el.findtext("octave") or "4").strip())
                    midi = (octave + 1) * 12 + STEP_TO_SEMITONE.get(step, 0) + alter

                    staff_num = int((elem.findtext("staff") or "1").strip())
                    voice_str = (elem.findtext("voice") or "1").strip()
                    ties = {t.attrib.get("type", "") for t in elem.findall("tie")}
                    tie_start = "start" in ties
                    tie_stop = "stop" in ties
                    tkey = (staff_num, voice_str, midi)

                    if tie_stop and tkey in pending_ties:
                        prev_m, prev_onset, prev_dur = pending_ties.pop(tkey)
                        total_dur = prev_dur + dur_beats
                        if tie_start:
                            pending_ties[tkey] = [prev_m, prev_onset, total_dur]
                        else:
                            notes.append(
                                (
                                    midi,
                                    int(prev_m),
                                    int(round(prev_onset * 480.0)),
                                    int(round(total_dur * 480.0)),
                                )
                            )
                    elif tie_start:
                        pending_ties[tkey] = [float(m_idx), onset, dur_beats]
                    else:
                        notes.append(
                            (
                                midi,
                                m_idx,
                                int(round(onset * 480.0)),
                                int(round(dur_beats * 480.0)),
                            )
                        )

            seen_first_measure_attrs = True

        for (_, _, midi), (pm, onset_b, dur_b) in pending_ties.items():
            notes.append((midi, int(pm), int(round(onset_b * 480.0)), int(round(dur_b * 480.0))))

        total_staves += part_staves
        for s in range(1, part_staves + 1):
            clefs.append(part_clefs.get(s, "G-2"))

    return {
        "staves": total_staves,
        "clefs": clefs,
        "notes": notes,
    }


def compute_note_f1(
    ref_notes: list[tuple[int, int, int, int]],
    hyp_notes: list[tuple[int, int, int, int]],
    onset_tol_ticks: int = 12,
    dur_tol_ticks: int = 24,
) -> tuple[float, float]:
    """Return (pitch_only_f1, strict_measure_pitch_onset_dur_f1)."""
    if not ref_notes and not hyp_notes:
        return 1.0, 1.0
    if not ref_notes or not hyp_notes:
        return 0.0, 0.0

    # Pitch multiset F1
    rc = collections.Counter(n[0] for n in ref_notes)
    hc = collections.Counter(n[0] for n in hyp_notes)
    pitch_tp = sum(min(rc[k], hc[k]) for k in rc)
    pitch_f1 = 2.0 * pitch_tp / (len(ref_notes) + len(hyp_notes))

    # Strict (midi, measure_idx, onset_in_measure, dur) greedy match within tolerance
    ref_by_key: dict[tuple[int, int], list[tuple[int, int]]] = collections.defaultdict(list)
    for m, bar, o, d in ref_notes:
        ref_by_key[(m, bar)].append((o, d))
    for v in ref_by_key.values():
        v.sort()

    hyp_by_key: dict[tuple[int, int], list[tuple[int, int]]] = collections.defaultdict(list)
    for m, bar, o, d in hyp_notes:
        hyp_by_key[(m, bar)].append((o, d))
    for v in hyp_by_key.values():
        v.sort()

    strict_tp = 0
    for key, hlist in hyp_by_key.items():
        rlist = ref_by_key.get(key)
        if not rlist:
            continue
        used = [False] * len(rlist)
        for ho, hd in hlist:
            best_i = -1
            best_err = 10**9
            for i, (ro, rd) in enumerate(rlist):
                if used[i]:
                    continue
                eo = abs(ho - ro)
                ed = abs(hd - rd)
                if eo <= onset_tol_ticks and ed <= dur_tol_ticks:
                    err = eo + ed
                    if err < best_err:
                        best_err = err
                        best_i = i
            if best_i >= 0:
                used[best_i] = True
                strict_tp += 1

    strict_f1 = 2.0 * strict_tp / (len(ref_notes) + len(hyp_notes))
    return pitch_f1, strict_f1


def check_verovio(xml_path: Path) -> tuple[bool, int, str]:
    """Render MusicXML via Verovio and return (ok, svg_note_count, detail)."""
    try:
        import verovio
    except ImportError:
        return False, 0, "verovio not installed"
    tk = verovio.toolkit()
    tk.setOptions({"breaks": "auto"})
    xml_text = xml_path.read_text(encoding="utf-8")
    loaded = tk.loadData(xml_text)
    if not loaded:
        return False, 0, "loadData failed"
    pages = tk.getPageCount()
    if pages < 1:
        return False, 0, "0 pages"
    svg = tk.renderToSVG(1)
    if "<svg" not in svg:
        return False, 0, "invalid svg"
    note_count = svg.count('class="note"')
    return True, note_count, f"{pages}p/{note_count}n"


def run_musescore_single(
    musescore_bin: Path, in_mscz: Path, out_xml: Path, work_dir: Path, idx: int
) -> tuple[bool, str]:
    """Run MuseScore 4 on a single .mscz with isolated TEMP/TMP shard.

    NEVER sets QT_QPA_PLATFORM=offscreen (Windows MuseScore 4 rule).
    """
    shard = work_dir / f"score-render-ms-{os.getpid()}-{idx}"
    shard.mkdir(parents=True, exist_ok=True)
    env = os.environ.copy()
    env.pop("QT_QPA_PLATFORM", None)
    env["TEMP"] = str(shard)
    env["TMP"] = str(shard)

    res = subprocess.run(
        [str(musescore_bin), "-o", str(out_xml), str(in_mscz)],
        env=env,
        capture_output=True,
        text=True,
        timeout=90,
    )
    if res.returncode != 0 or not out_xml.exists():
        return False, f"exit {res.returncode}"
    return True, "ok"


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument(
        "--corpus-dir",
        type=Path,
        default=Path(
            r"C:\Users\grego\projects\sheet-music-converter\data-gen\score-corpus\suite31\gt"
        ),
    )
    ap.add_argument(
        "--mscz-bin",
        type=Path,
        default=Path(__file__).resolve().parent.parent / "target" / "debug" / "mscz.exe",
    )
    ap.add_argument(
        "--musescore-bin",
        type=Path,
        default=Path(r"C:\Program Files\MuseScore 4\bin\MuseScore4.exe"),
    )
    ap.add_argument("--pieces", nargs="*", default=DEFAULT_PIECES)
    ap.add_argument("--skip-musescore", action="store_true")
    args = ap.parse_args()

    if not args.mscz_bin.exists():
        print(f"ERROR: mscz binary not found at {args.mscz_bin}", file=sys.stderr)
        return 2

    with tempfile.TemporaryDirectory(prefix="mscz_parity_") as td:
        work = Path(td)
        records = []
        ms4_jobs = []

        for piece in args.pieces:
            gt_xml = args.corpus_dir / f"gt-p1-{piece}.musicxml"
            if not gt_xml.exists():
                print(f"SKIP missing {gt_xml}", file=sys.stderr)
                continue

            out_dir = work / piece
            out_dir.mkdir(parents=True, exist_ok=True)

            # 1. GT MusicXML -> bekern+
            r1 = subprocess.run(
                [
                    str(args.mscz_bin),
                    "bekern",
                    "--input",
                    str(gt_xml),
                    "--out",
                    str(out_dir),
                    "--page",
                    "1",
                    "--stem",
                    piece,
                ],
                capture_output=True,
                text=True,
            )
            if r1.returncode != 0:
                records.append({"piece": piece, "error": f"bekern failed: {r1.stderr.strip()}"})
                continue

            bekern_file = out_dir / f"{piece}-bekern.txt"
            # 2. bekern+ -> .musicxml + .mscz
            r2 = subprocess.run(
                [
                    str(args.mscz_bin),
                    "bekern2musicxml",
                    "--input",
                    str(bekern_file),
                    "--out",
                    str(out_dir),
                    "--stem",
                    piece,
                ],
                capture_output=True,
                text=True,
            )
            if r2.returncode != 0:
                records.append(
                    {"piece": piece, "error": f"bekern2musicxml failed: {r2.stderr.strip()}"}
                )
                continue

            rs_xml = out_dir / f"{piece}.musicxml"
            rs_mscz = out_dir / f"{piece}.mscz"
            ms4_xml = out_dir / f"{piece}.ms4.musicxml"

            if not args.skip_musescore and args.musescore_bin.exists():
                ms4_jobs.append((len(records), rs_mscz, ms4_xml))

            records.append(
                {
                    "piece": piece,
                    "gt_xml": gt_xml,
                    "rs_xml": rs_xml,
                    "rs_mscz": rs_mscz,
                    "ms4_xml": ms4_xml,
                    "ms4_err": None,
                }
            )

        for idx, rs_mscz, ms4_xml in ms4_jobs:
            ok, msg = run_musescore_single(args.musescore_bin, rs_mscz, ms4_xml, work, idx)
            if not ok:
                records[idx]["ms4_err"] = msg

        # Evaluate & print Markdown table
        print(
            "| Piece | GT Staves / Clefs | mscz-rs Staves / Clefs | MS4 Export Staves / Clefs | "
            "mscz-rs vs GT F1 (Strict/Pitch) | MS4 vs mscz-rs F1 (Strict/Pitch) | Verovio SVG | Parity |"
        )
        print("|---|---|---|---|---|---|---|---|")

        all_pass = True
        for rec in records:
            piece = rec["piece"]
            if "error" in rec:
                print(f"| {piece} | - | - | - | - | - | - | FAIL ({rec['error']}) |")
                all_pass = False
                continue

            gt = parse_musicxml_summary(rec["gt_xml"])
            rs = parse_musicxml_summary(rec["rs_xml"])
            rs_p_f1, rs_s_f1 = compute_note_f1(gt["notes"], rs["notes"])

            if rec.get("ms4_err"):
                ms4_staves_str = f"ERR ({rec['ms4_err']})"
                ms4_f1_str = "ERR"
                ms4_ok = False
            elif rec["ms4_xml"].exists():
                ms4 = parse_musicxml_summary(rec["ms4_xml"])
                ms4_p_f1, ms4_s_f1 = compute_note_f1(rs["notes"], ms4["notes"])
                ms4_staves_str = f"{ms4['staves']} ({','.join(ms4['clefs'])})"
                ms4_f1_str = f"{ms4_s_f1:.4f} / {ms4_p_f1:.4f}"
                ms4_ok = (
                    ms4["staves"] == gt["staves"]
                    and ms4["clefs"] == gt["clefs"]
                    and ms4_p_f1 >= 0.99
                    and ms4_s_f1 >= 0.95
                )
            else:
                ms4_staves_str = "SKIP"
                ms4_f1_str = "SKIP"
                ms4_ok = True

            v_ok, _, v_detail = check_verovio(rec["rs_xml"])
            rs_ok = (
                rs["staves"] == gt["staves"]
                and rs["clefs"] == gt["clefs"]
                and rs_p_f1 >= 0.99
                and rs_s_f1 >= 0.95
            )
            passed = rs_ok and ms4_ok and v_ok
            if not passed:
                all_pass = False

            gt_str = f"{gt['staves']} ({','.join(gt['clefs'])})"
            rs_str = f"{rs['staves']} ({','.join(rs['clefs'])})"
            status = "PASS" if passed else "FAIL"
            print(
                f"| {piece} | {gt_str} | {rs_str} | {ms4_staves_str} | "
                f"{rs_s_f1:.4f} / {rs_p_f1:.4f} | {ms4_f1_str} | {'OK' if v_ok else 'FAIL'} ({v_detail}) | {status} |"
            )

    return 0 if all_pass else 1


if __name__ == "__main__":
    raise SystemExit(main())
