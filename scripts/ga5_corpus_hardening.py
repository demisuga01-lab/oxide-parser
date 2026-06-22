#!/usr/bin/env python3
"""Run GA5 cross-pillar robustness smoke over a PDF manifest.

The harness intentionally treats malformed-input failures as acceptable clean
errors. The release-safety bar here is: no crash/abort, no timeout/hang, and
writer outputs from qpdf-clean inputs must pass `qpdf --check` when qpdf is
present. Outputs from inputs that qpdf already repairs/rejects are reported
separately as inherited source damage.
"""

from __future__ import annotations

import argparse
import json
import shutil
import subprocess
import time
from dataclasses import dataclass, asdict
from pathlib import Path
from typing import Any


CRASH_EXIT_MIN = 0xC0000000


@dataclass
class CommandResult:
    operation: str
    status: str
    exit_code: int | None
    duration_ms: int
    output_valid: bool | None = None
    output_check_scope: str | None = None
    stderr_tail: str = ""


def run_command(cmd: list[str], timeout: int) -> tuple[str, int | None, int, str]:
    started = time.perf_counter()
    try:
        completed = subprocess.run(
            cmd,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            timeout=timeout,
            check=False,
        )
        duration_ms = round((time.perf_counter() - started) * 1000)
        code = completed.returncode
        stderr = completed.stderr.decode("utf-8", errors="replace")[-600:]
        if code == 0:
            return "ok", code, duration_ms, stderr
        if code < 0 or code >= CRASH_EXIT_MIN:
            return "crash", code, duration_ms, stderr
        return "clean_error", code, duration_ms, stderr
    except subprocess.TimeoutExpired as err:
        duration_ms = round((time.perf_counter() - started) * 1000)
        stderr = (err.stderr or b"").decode("utf-8", errors="replace")[-600:]
        return "timeout", None, duration_ms, stderr


def qpdf_check(path: Path, timeout: int) -> bool | None:
    qpdf = shutil.which("qpdf")
    if not qpdf or not path.exists():
        return None
    status, _, _, _ = run_command([qpdf, "--check", str(path)], timeout)
    return status == "ok"


def selected_entries(manifest: dict[str, Any], limit: int, include_hostile: bool) -> list[dict[str, Any]]:
    entries = manifest.get("entries", [])
    if not include_hostile:
        entries = [
            entry
            for entry in entries
            if not str(entry.get("category", "")).startswith("hostile-")
        ]
    return entries[:limit]


def run_entry(entry: dict[str, Any], oxide: Path, out_dir: Path, timeout: int) -> dict[str, Any]:
    pdf = Path(entry["path"])
    safe_id = str(entry.get("id") or pdf.stem).replace("/", "_").replace("\\", "_")
    work = out_dir / safe_id
    work.mkdir(parents=True, exist_ok=True)
    operations: list[CommandResult] = []
    input_qpdf_clean = qpdf_check(pdf, timeout)

    commands = {
        "info": [str(oxide), "info", "--json", str(pdf)],
        "parse": [str(oxide), "parse", "-f", "json", "-o", str(work / "parse.json"), str(pdf)],
        "verify_sig": [str(oxide), "verify-sig", "--json", str(pdf)],
        "render_p1": [
            str(oxide),
            "render",
            "-p",
            "1",
            "-d",
            "72",
            "-o",
            str(work / "page1.zip"),
            str(pdf),
        ],
        "optimize": [str(oxide), "optimize", "-o", str(work / "optimized.pdf"), str(pdf)],
        "linearize": [str(oxide), "linearize", "-o", str(work / "linearized.pdf"), str(pdf)],
    }

    for name, cmd in commands.items():
        status, code, duration_ms, stderr = run_command(cmd, timeout)
        output_valid: bool | None = None
        output_check_scope: str | None = None
        if name == "optimize" and status == "ok":
            output_valid = qpdf_check(work / "optimized.pdf", timeout)
            output_check_scope = "clean_input" if input_qpdf_clean else "source_not_qpdf_clean"
        elif name == "linearize" and status == "ok":
            output_valid = qpdf_check(work / "linearized.pdf", timeout)
            output_check_scope = "clean_input" if input_qpdf_clean else "source_not_qpdf_clean"
        operations.append(
            CommandResult(
                operation=name,
                status=status,
                exit_code=code,
                duration_ms=duration_ms,
                output_valid=output_valid,
                output_check_scope=output_check_scope,
                stderr_tail=stderr,
            )
        )

    return {
        "id": safe_id,
        "path": str(pdf),
        "category": entry.get("category"),
        "input_qpdf_clean": input_qpdf_clean,
        "operations": [asdict(op) for op in operations],
    }


def summarize(results: list[dict[str, Any]]) -> dict[str, Any]:
    totals: dict[str, dict[str, int]] = {}
    input_qpdf = {"clean": 0, "not_clean": 0, "not_checked": 0}
    output_checks = {
        "checked": 0,
        "passed": 0,
        "failed_clean_input": 0,
        "inherited_source_not_qpdf_clean": 0,
        "failed_unknown_input_state": 0,
    }
    for item in results:
        if item.get("input_qpdf_clean") is True:
            input_qpdf["clean"] += 1
        elif item.get("input_qpdf_clean") is False:
            input_qpdf["not_clean"] += 1
        else:
            input_qpdf["not_checked"] += 1
        for op in item["operations"]:
            bucket = totals.setdefault(op["operation"], {"ok": 0, "clean_error": 0, "timeout": 0, "crash": 0})
            bucket[op["status"]] = bucket.get(op["status"], 0) + 1
            if op["output_valid"] is not None:
                output_checks["checked"] += 1
                if op["output_valid"]:
                    output_checks["passed"] += 1
                elif op.get("output_check_scope") == "source_not_qpdf_clean":
                    output_checks["inherited_source_not_qpdf_clean"] += 1
                elif op.get("output_check_scope") == "clean_input":
                    output_checks["failed_clean_input"] += 1
                else:
                    output_checks["failed_unknown_input_state"] += 1
    crashes = sum(op_counts.get("crash", 0) for op_counts in totals.values())
    timeouts = sum(op_counts.get("timeout", 0) for op_counts in totals.values())
    return {
        "files": len(results),
        "operations": totals,
        "input_qpdf": input_qpdf,
        "crashes": crashes,
        "timeouts": timeouts,
        "output_checks": output_checks,
        "crash_free_percent": round(100.0 if not results else 100.0 * (1 - crashes / (len(results) * len(totals))), 2),
        "timeout_free_percent": round(100.0 if not results else 100.0 * (1 - timeouts / (len(results) * len(totals))), 2),
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", default="renderer-benchmark/corpus/manifest.json")
    parser.add_argument("--oxide-bin", default="target/release/oxide.exe")
    parser.add_argument("--output-dir", default="target/ga5-corpus-hardening")
    parser.add_argument("--limit", type=int, default=80)
    parser.add_argument("--timeout-sec", type=int, default=20)
    parser.add_argument("--include-hostile", action="store_true")
    args = parser.parse_args()

    manifest = json.loads(Path(args.manifest).read_text(encoding="utf-8"))
    entries = selected_entries(manifest, args.limit, args.include_hostile)
    out_dir = Path(args.output_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    oxide = Path(args.oxide_bin)

    results = []
    for idx, entry in enumerate(entries, start=1):
        print(f"[{idx}/{len(entries)}] {entry.get('id')} ({entry.get('category')})", flush=True)
        result = run_entry(entry, oxide, out_dir, args.timeout_sec)
        results.append(result)
        (out_dir / f"{result['id']}.json").write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")

    aggregate = {
        "manifest": args.manifest,
        "oxide_bin": args.oxide_bin,
        "limit": args.limit,
        "timeout_sec": args.timeout_sec,
        "include_hostile": args.include_hostile,
        "summary": summarize(results),
        "results": results,
    }
    (out_dir / "aggregate.json").write_text(json.dumps(aggregate, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(aggregate["summary"], indent=2))


if __name__ == "__main__":
    main()
