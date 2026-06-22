#!/usr/bin/env python3
"""Differential fuzzing harness for Oxide.

The harness intentionally checks high-signal properties first: qpdf/page-count
agreement, Poppler/Oxide text extraction at a tolerant token level, and writer
round-trip validity. It is designed for CI smoke runs and longer scheduled runs.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import random
import re
import shutil
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path


DEFAULT_SEED_DIRS = [
    Path("crates/engine/tests/fixtures"),
    Path("tests/corpus/pdfs/existing"),
    Path("tests/corpus/pdfs/generated"),
]


@dataclass
class Run:
    code: int
    stdout: str
    stderr: str
    timed_out: bool = False


def run_cmd(args: list[str], timeout: int = 20) -> Run:
    try:
        proc = subprocess.run(
            args,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=timeout,
            check=False,
        )
        return Run(proc.returncode, proc.stdout, proc.stderr)
    except subprocess.TimeoutExpired as exc:
        return Run(124, exc.stdout or "", exc.stderr or "", timed_out=True)


def resolve_tool(explicit: str | None, candidates: list[str]) -> str:
    if explicit:
        return explicit
    for candidate in candidates:
        if Path(candidate).exists() or shutil.which(candidate):
            return candidate
    raise SystemExit(f"required tool not found; tried: {', '.join(candidates)}")


def discover_seed_pdfs(seed_dirs: list[Path], max_bytes: int, limit: int) -> list[Path]:
    seen: set[Path] = set()
    pdfs: list[Path] = []
    for seed_dir in seed_dirs:
        if not seed_dir.exists():
            continue
        for pdf in sorted(seed_dir.rglob("*.pdf")):
            try:
                if pdf.stat().st_size > max_bytes:
                    continue
            except OSError:
                continue
            resolved = pdf.resolve()
            if resolved not in seen:
                seen.add(resolved)
                pdfs.append(pdf)
    return pdfs[:limit]


def escape_pdf_text(text: str) -> str:
    return text.replace("\\", "\\\\").replace("(", "\\(").replace(")", "\\)")


def build_minimal_pdf(text: str) -> bytes:
    content = f"BT /F1 12 Tf 72 72 Td ({escape_pdf_text(text)}) Tj ET\n".encode("ascii")
    objects = [
        b"<< /Type /Catalog /Pages 2 0 R >>\n",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>\n",
        (
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 144] "
            b"/Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>\n"
        ),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\n",
        b"<< /Length " + str(len(content)).encode("ascii") + b" >>\nstream\n" + content + b"endstream\n",
    ]
    out = bytearray(b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n")
    offsets = [0]
    for index, obj in enumerate(objects, start=1):
        offsets.append(len(out))
        out.extend(f"{index} 0 obj\n".encode("ascii"))
        out.extend(obj)
        out.extend(b"endobj\n")
    xref = len(out)
    out.extend(f"xref\n0 {len(objects) + 1}\n".encode("ascii"))
    out.extend(b"0000000000 65535 f \n")
    for offset in offsets[1:]:
        out.extend(f"{offset:010d} 00000 n \n".encode("ascii"))
    out.extend(
        f"trailer\n<< /Size {len(objects) + 1} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n".encode(
            "ascii"
        )
    )
    return bytes(out)


def mutate_pdf(data: bytes, rng: random.Random) -> bytes:
    if len(data) < 32:
        return data
    mutated = bytearray(data)
    operations = rng.randint(1, 3)
    low = min(len(mutated) - 1, 8)
    for _ in range(operations):
        choice = rng.choice(["flip", "insert", "delete"])
        pos = rng.randrange(low, len(mutated))
        if choice == "flip":
            mutated[pos] ^= 1 << rng.randrange(0, 8)
        elif choice == "insert" and len(mutated) < len(data) + 64:
            mutated[pos:pos] = bytes([rng.randrange(0, 256)])
        elif choice == "delete" and len(mutated) > 32:
            del mutated[pos]
    return bytes(mutated)


def normalize_text(text: str) -> list[str]:
    return re.findall(r"[\w]+", text.casefold())


def token_similarity(left: list[str], right: list[str]) -> float:
    if not left and not right:
        return 1.0
    if not left or not right:
        return 0.0
    lset = set(left)
    rset = set(right)
    overlap = len(lset & rset)
    return (2.0 * overlap) / (len(lset) + len(rset))


def qpdf_page_count(qpdf: str, pdf: Path) -> tuple[int | None, Run]:
    run = run_cmd([qpdf, "--show-npages", str(pdf)], timeout=15)
    if run.code != 0:
        return None, run
    try:
        return int(run.stdout.strip()), run
    except ValueError:
        return None, run


def oxide_info(oxide: str, pdf: Path) -> tuple[dict | None, Run]:
    run = run_cmd([oxide, "info", "--json", str(pdf)], timeout=15)
    if run.code != 0:
        return None, run
    try:
        return json.loads(run.stdout), run
    except json.JSONDecodeError:
        return None, run


def qpdf_check(qpdf: str, pdf: Path) -> Run:
    return run_cmd([qpdf, "--check", str(pdf)], timeout=20)


def extract_oxide_text(oxide: str, pdf: Path) -> Run:
    return run_cmd([oxide, "extract-text", str(pdf)], timeout=20)


def extract_poppler_text(pdftotext: str, pdf: Path, out_dir: Path) -> Run:
    out = out_dir / f"{pdf.stem}.poppler.txt"
    run = run_cmd([pdftotext, "-enc", "UTF-8", str(pdf), str(out)], timeout=20)
    if run.code == 0:
        try:
            run.stdout = out.read_text(encoding="utf-8", errors="replace")
        except OSError as exc:
            run.code = 1
            run.stderr = str(exc)
    return run


def save_json(path: Path, data: object) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(data, indent=2, sort_keys=True), encoding="utf-8")


def case_id(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()[:16]


def write_case(path: Path, data: bytes) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)
    return path


def evaluate_case(
    *,
    pdf: Path,
    output: Path,
    oxide: str,
    qpdf: str,
    pdftotext: str,
    text_threshold: float,
    writer_roundtrip: bool,
) -> tuple[list[dict], list[dict]]:
    findings: list[dict] = []
    notes: list[dict] = []

    qcheck = qpdf_check(qpdf, pdf)
    qpdf_valid = qcheck.code == 0
    qpages, qpages_run = qpdf_page_count(qpdf, pdf)
    oinfo, oinfo_run = oxide_info(oxide, pdf)

    if qpdf_valid and oinfo is None:
        findings.append(
            {
                "kind": "qpdf_valid_oxide_rejected",
                "case": str(pdf),
                "oxide_error": oinfo_run.stderr.strip() or oinfo_run.stdout.strip(),
            }
        )
    elif not qpdf_valid and oinfo is not None:
        notes.append(
            {
                "kind": "oxide_lenient_reference_rejected",
                "case": str(pdf),
                "qpdf_error": qcheck.stderr.strip() or qcheck.stdout.strip(),
            }
        )

    if qpages is not None and oinfo is not None:
        oxide_pages = int(oinfo.get("page_count", -1))
        if oxide_pages != qpages:
            findings.append(
                {
                    "kind": "page_count_mismatch",
                    "case": str(pdf),
                    "qpdf_pages": qpages,
                    "oxide_pages": oxide_pages,
                }
            )
    elif qpdf_valid and qpages is None:
        findings.append(
            {
                "kind": "qpdf_valid_no_reference_page_count",
                "case": str(pdf),
                "qpdf_output": qpages_run.stderr.strip() or qpages_run.stdout.strip(),
            }
        )

    if qpdf_valid and oinfo is not None:
        oxide_text = extract_oxide_text(oxide, pdf)
        poppler_text = extract_poppler_text(pdftotext, pdf, output / "text")
        if oxide_text.code == 0 and poppler_text.code == 0:
            left = normalize_text(oxide_text.stdout)
            right = normalize_text(poppler_text.stdout)
            similarity = token_similarity(left, right)
            if similarity < text_threshold:
                findings.append(
                    {
                        "kind": "text_token_divergence",
                        "case": str(pdf),
                        "similarity": similarity,
                        "oxide_tokens": left[:80],
                        "poppler_tokens": right[:80],
                    }
                )
        elif poppler_text.code == 0 and oxide_text.code != 0:
            findings.append(
                {
                    "kind": "qpdf_valid_poppler_text_ok_oxide_text_failed",
                    "case": str(pdf),
                    "oxide_error": oxide_text.stderr.strip() or oxide_text.stdout.strip(),
                }
            )
        else:
            notes.append(
                {
                    "kind": "text_diff_skipped",
                    "case": str(pdf),
                    "oxide_code": oxide_text.code,
                    "poppler_code": poppler_text.code,
                }
            )

    if writer_roundtrip and qpdf_valid and oinfo is not None:
        roundtrip = output / "roundtrip" / f"{pdf.stem}.optimized.pdf"
        roundtrip.parent.mkdir(parents=True, exist_ok=True)
        opt = run_cmd([oxide, "optimize", "-o", str(roundtrip), str(pdf)], timeout=30)
        if opt.code != 0:
            findings.append(
                {
                    "kind": "writer_roundtrip_failed",
                    "case": str(pdf),
                    "oxide_error": opt.stderr.strip() or opt.stdout.strip(),
                }
            )
        else:
            out_check = qpdf_check(qpdf, roundtrip)
            if out_check.code != 0:
                findings.append(
                    {
                        "kind": "writer_output_qpdf_invalid",
                        "case": str(pdf),
                        "output": str(roundtrip),
                        "qpdf_error": out_check.stderr.strip() or out_check.stdout.strip(),
                    }
                )
            out_pages, _ = qpdf_page_count(qpdf, roundtrip)
            if qpages is not None and out_pages is not None and out_pages != qpages:
                findings.append(
                    {
                        "kind": "writer_roundtrip_page_count_mismatch",
                        "case": str(pdf),
                        "output": str(roundtrip),
                        "original_pages": qpages,
                        "output_pages": out_pages,
                    }
                )

    return findings, notes


def build_cases(args: argparse.Namespace, rng: random.Random, output: Path) -> list[Path]:
    case_paths: list[Path] = []
    regression_dir = Path(args.regressions)
    if regression_dir.exists():
        for pdf in sorted(regression_dir.rglob("*.pdf")):
            case_paths.append(pdf)

    seed_dirs = [Path(item) for item in args.seed_dir]
    seeds = discover_seed_pdfs(seed_dirs, args.max_seed_bytes, args.seed_limit)
    selected = seeds[: max(0, args.cases)]
    for index, seed in enumerate(selected):
        data = seed.read_bytes()
        exact_name = f"{index:04d}-{seed.stem}-exact-{case_id(data)}.pdf"
        case_paths.append(write_case(output / "inputs" / exact_name, data))
        if len(case_paths) >= args.cases:
            break
        mutated = mutate_pdf(data, rng)
        mut_name = f"{index:04d}-{seed.stem}-mut-{case_id(mutated)}.pdf"
        case_paths.append(write_case(output / "inputs" / mut_name, mutated))
        if len(case_paths) >= args.cases:
            break

    while len(case_paths) < args.cases:
        text = f"oxide differential case {len(case_paths)} seed {args.seed}"
        data = build_minimal_pdf(text)
        name = f"grammar-{len(case_paths):04d}-{case_id(data)}.pdf"
        case_paths.append(write_case(output / "inputs" / name, data))

    return case_paths


def main() -> int:
    parser = argparse.ArgumentParser(description="Differential fuzz Oxide against qpdf/Poppler.")
    parser.add_argument("--cases", type=int, default=20, help="Number of generated/mutated cases.")
    parser.add_argument("--seed", type=int, default=0x0D1F_F00D, help="Deterministic RNG seed.")
    parser.add_argument("--seed-dir", action="append", default=[], help="Directory containing seed PDFs.")
    parser.add_argument("--seed-limit", type=int, default=64, help="Maximum seed PDFs to consider.")
    parser.add_argument("--max-seed-bytes", type=int, default=2_000_000, help="Skip larger seed PDFs.")
    parser.add_argument("--output", default="target/differential-fuzz", help="Output directory.")
    parser.add_argument("--regressions", default="differential/regressions", help="Regression seed dir.")
    parser.add_argument("--oxide", default=None, help="Path to oxide CLI.")
    parser.add_argument("--qpdf", default=None, help="Path to qpdf.")
    parser.add_argument("--pdftotext", default=None, help="Path to Poppler pdftotext.")
    parser.add_argument("--text-threshold", type=float, default=0.65, help="Token similarity floor.")
    parser.add_argument("--no-writer-roundtrip", action="store_true", help="Skip optimize round-trip.")
    args = parser.parse_args()

    if not args.seed_dir:
        args.seed_dir = [str(path) for path in DEFAULT_SEED_DIRS]

    output = Path(args.output)
    output.mkdir(parents=True, exist_ok=True)
    oxide = resolve_tool(args.oxide, ["target/debug/oxide.exe", "target/release/oxide.exe", "oxide"])
    qpdf = resolve_tool(args.qpdf, ["qpdf"])
    pdftotext = resolve_tool(args.pdftotext, ["pdftotext"])

    rng = random.Random(args.seed)
    cases = build_cases(args, rng, output)
    all_findings: list[dict] = []
    all_notes: list[dict] = []
    for pdf in cases:
        findings, notes = evaluate_case(
            pdf=pdf,
            output=output,
            oxide=oxide,
            qpdf=qpdf,
            pdftotext=pdftotext,
            text_threshold=args.text_threshold,
            writer_roundtrip=not args.no_writer_roundtrip,
        )
        all_findings.extend(findings)
        all_notes.extend(notes)

    report = {
        "cases": len(cases),
        "findings": all_findings,
        "notes": all_notes,
        "tools": {
            "oxide": oxide,
            "qpdf": qpdf,
            "pdftotext": pdftotext,
        },
        "seed": args.seed,
        "text_threshold": args.text_threshold,
        "writer_roundtrip": not args.no_writer_roundtrip,
    }
    save_json(output / "report.json", report)
    if all_findings:
        save_json(output / "findings" / "findings.json", all_findings)
        print(f"differential fuzz found {len(all_findings)} high-signal disagreement(s)")
        print(f"report: {output / 'report.json'}")
        return 1

    print(f"differential fuzz clean: {len(cases)} cases, {len(all_notes)} accepted note(s)")
    print(f"report: {output / 'report.json'}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
