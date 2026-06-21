#!/usr/bin/env python3
"""Prompt H Oxide-vs-Poppler performance proof harness.

This is intentionally separate from the renderer visual benchmark. It measures
equivalent CLI work on the same PDFs with release binaries:

* open/info latency
* first-page text/render latency
* all-page text/render throughput where reasonable
* image extraction
* peak working set / RSS
* Oxide single-thread and multi-thread, with Poppler as the single-thread CLI

Outputs:
  docs/perf_prompt_h_results.json
  docs/perf_prompt_h_summary.md
"""

from __future__ import annotations

import argparse
import json
import os
import platform
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
IS_WINDOWS = platform.system() == "Windows"


if IS_WINDOWS:
    import ctypes
    from ctypes import wintypes

    class PROCESS_MEMORY_COUNTERS(ctypes.Structure):
        _fields_ = [
            ("cb", wintypes.DWORD),
            ("PageFaultCount", wintypes.DWORD),
            ("PeakWorkingSetSize", ctypes.c_size_t),
            ("WorkingSetSize", ctypes.c_size_t),
            ("QuotaPeakPagedPoolUsage", ctypes.c_size_t),
            ("QuotaPagedPoolUsage", ctypes.c_size_t),
            ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t),
            ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
            ("PagefileUsage", ctypes.c_size_t),
            ("PeakPagefileUsage", ctypes.c_size_t),
        ]

    _psapi = ctypes.WinDLL("psapi", use_last_error=True)
    _kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    PROCESS_QUERY_INFORMATION = 0x0400
    PROCESS_VM_READ = 0x0010

    def run_and_measure(cmd: list[str], env: dict[str, str], cwd: Path, timeout: int) -> dict:
        start = time.perf_counter()
        proc = subprocess.Popen(
            cmd,
            env=env,
            cwd=str(cwd),
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        handle = _kernel32.OpenProcess(
            PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, False, proc.pid
        )
        peak = 0
        counters = PROCESS_MEMORY_COUNTERS()
        counters.cb = ctypes.sizeof(PROCESS_MEMORY_COUNTERS)
        timed_out = False
        try:
            while proc.poll() is None:
                if time.perf_counter() - start > timeout:
                    timed_out = True
                    proc.kill()
                    break
                if handle and _psapi.GetProcessMemoryInfo(
                    handle, ctypes.byref(counters), counters.cb
                ):
                    peak = max(peak, counters.PeakWorkingSetSize)
                time.sleep(0.002)
            if handle and _psapi.GetProcessMemoryInfo(
                handle, ctypes.byref(counters), counters.cb
            ):
                peak = max(peak, counters.PeakWorkingSetSize)
        finally:
            if handle:
                _kernel32.CloseHandle(handle)
        rc = proc.wait()
        return {
            "time_s": time.perf_counter() - start,
            "peak_bytes": peak,
            "rc": rc,
            "timed_out": timed_out,
        }

else:
    import threading

    def _sample_rss(pid: int, stop_evt: threading.Event, out: list[int]) -> None:
        status = Path(f"/proc/{pid}/status")
        while not stop_evt.is_set():
            try:
                for line in status.read_text().splitlines():
                    if line.startswith("VmHWM:"):
                        out[0] = max(out[0], int(line.split()[1]) * 1024)
            except (FileNotFoundError, ProcessLookupError, ValueError):
                pass
            time.sleep(0.002)

    def run_and_measure(cmd: list[str], env: dict[str, str], cwd: Path, timeout: int) -> dict:
        start = time.perf_counter()
        proc = subprocess.Popen(
            cmd,
            env=env,
            cwd=str(cwd),
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        out = [0]
        stop = threading.Event()
        sampler = threading.Thread(target=_sample_rss, args=(proc.pid, stop, out))
        sampler.start()
        timed_out = False
        try:
            try:
                rc = proc.wait(timeout=timeout)
            except subprocess.TimeoutExpired:
                timed_out = True
                proc.kill()
                rc = proc.wait()
        finally:
            stop.set()
            sampler.join()
        return {
            "time_s": time.perf_counter() - start,
            "peak_bytes": out[0],
            "rc": rc,
            "timed_out": timed_out,
        }


def exe(name: str) -> str:
    return f"{name}.exe" if IS_WINDOWS and not name.endswith(".exe") else name


def mb(value: int | float | None) -> float | None:
    if value is None:
        return None
    return value / (1024 * 1024)


@dataclass(frozen=True)
class Case:
    key: str
    doc_class: str
    path: str
    ops: tuple[str, ...]


CASES: tuple[Case, ...] = (
    Case(
        "small_1p_text",
        "1-page small text",
        "tests/corpus/pdfs/generated/generated_basic_text.pdf",
        ("info", "text_page1", "text_all", "render_page1", "render_all"),
    ),
    Case(
        "multi_120p_text",
        "100+ page text",
        "tests/corpus/pdfs/generated/generated_120_pages.pdf",
        ("info", "text_page1", "text_all", "render_page1", "render_all"),
    ),
    Case(
        "image_heavy",
        "image-heavy scanned",
        "tests/corpus/pdfs/pdfjs/images.pdf",
        ("info", "render_page1", "render_all", "images"),
    ),
    Case(
        "vector_heavy",
        "vector-heavy",
        "tests/corpus/pdfs/generated/generated_vector_bars.pdf",
        ("info", "render_page1", "render_all"),
    ),
    Case(
        "font_heavy",
        "font-heavy",
        "tests/corpus/pdfs/pdfjs/mixedfonts.pdf",
        ("info", "text_page1", "text_all", "render_page1", "render_all"),
    ),
    Case(
        "large_linearized",
        "large-byte linearized",
        "tests/corpus/pdfs/pdfjs/freeculture.pdf",
        ("info", "text_page1", "text_all", "render_page1"),
    ),
    Case(
        "incremental_xfa",
        "incremental/XFA form",
        "renderer-benchmark/corpus/real-world/pdfjs-full/xfa_filled_imm1344e.pdf",
        ("info", "text_page1", "text_all", "render_page1", "render_all"),
    ),
)


def read_page_count(oxide_bin: Path, pdf: Path) -> int | None:
    try:
        proc = subprocess.run(
            [str(oxide_bin), "info", str(pdf), "--json"],
            cwd=REPO_ROOT,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            timeout=20,
            check=False,
        )
        if proc.returncode != 0:
            return None
        return int(json.loads(proc.stdout).get("page_count"))
    except Exception:
        return None


def command_builder(
    engine: str,
    op: str,
    pdf: Path,
    oxide_bin: Path,
    poppler: dict[str, Path],
    dpi: int,
):
    def build(td: Path) -> list[str]:
        if engine == "oxide":
            if op == "info":
                return [str(oxide_bin), "info", str(pdf), "--json"]
            if op == "text_page1":
                return [
                    str(oxide_bin),
                    "extract-text",
                    str(pdf),
                    "--pages",
                    "1",
                    "--output",
                    str(td / "oxide.txt"),
                ]
            if op == "text_all":
                return [
                    str(oxide_bin),
                    "extract-text",
                    str(pdf),
                    "--output",
                    str(td / "oxide.txt"),
                ]
            if op == "render_page1":
                return [
                    str(oxide_bin),
                    "render",
                    str(pdf),
                    "--pages",
                    "1",
                    "--dpi",
                    str(dpi),
                    "--format",
                    "png",
                    "--output",
                    str(td / "oxide.zip"),
                ]
            if op == "render_all":
                return [
                    str(oxide_bin),
                    "render",
                    str(pdf),
                    "--dpi",
                    str(dpi),
                    "--format",
                    "png",
                    "--output",
                    str(td / "oxide.zip"),
                ]
            if op == "images":
                return [
                    str(oxide_bin),
                    "extract-images",
                    str(pdf),
                    "--format",
                    "original",
                    "--output",
                    str(td / "oxide.zip"),
                ]
        else:
            if op == "info":
                return [str(poppler["pdfinfo"]), str(pdf)]
            if op == "text_page1":
                return [
                    str(poppler["pdftotext"]),
                    "-enc",
                    "UTF-8",
                    "-nopgbrk",
                    "-f",
                    "1",
                    "-l",
                    "1",
                    str(pdf),
                    str(td / "poppler.txt"),
                ]
            if op == "text_all":
                return [
                    str(poppler["pdftotext"]),
                    "-enc",
                    "UTF-8",
                    "-nopgbrk",
                    str(pdf),
                    str(td / "poppler.txt"),
                ]
            if op == "render_page1":
                return [
                    str(poppler["pdftoppm"]),
                    "-r",
                    str(dpi),
                    "-f",
                    "1",
                    "-l",
                    "1",
                    str(pdf),
                    str(td / "page"),
                ]
            if op == "render_all":
                return [
                    str(poppler["pdftoppm"]),
                    "-r",
                    str(dpi),
                    str(pdf),
                    str(td / "page"),
                ]
            if op == "images":
                return [str(poppler["pdfimages"]), str(pdf), str(td / "image")]
        raise ValueError(f"unsupported op {engine=} {op=}")

    return build


def measure(
    build,
    threads: int | None,
    repeats: int,
    timeout: int,
) -> dict:
    env = dict(os.environ)
    if threads is not None:
        env["RAYON_NUM_THREADS"] = str(threads)
    runs = []
    for _ in range(repeats):
        with tempfile.TemporaryDirectory() as tmp:
            runs.append(run_and_measure(build(Path(tmp)), env, REPO_ROOT, timeout))
    times = [r["time_s"] for r in runs]
    peaks = [r["peak_bytes"] for r in runs]
    return {
        "runs": runs,
        "median_s": statistics.median(times),
        "cold_s": times[0],
        "warm_median_s": statistics.median(times[1:]) if len(times) > 1 else None,
        "peak_mb": mb(max(peaks)),
        "rcs": [r["rc"] for r in runs],
        "timed_out": any(r["timed_out"] for r in runs),
    }


def version_text(cmd: list[str]) -> str:
    try:
        proc = subprocess.run(
            cmd,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            timeout=10,
            check=False,
        )
        return proc.stdout.strip()
    except Exception as err:
        return str(err)


def summarize_ratios(rows: list[dict]) -> list[dict]:
    by_key: dict[tuple[str, str], dict[str, dict]] = {}
    for row in rows:
        by_key.setdefault((row["case"], row["op"]), {})[row["engine_label"]] = row
    ratios = []
    for (case, op), group in sorted(by_key.items()):
        pop = group.get("poppler")
        if not pop or pop["median_s"] <= 0:
            continue
        for label in ("oxide@1", "oxide@N"):
            ox = group.get(label)
            if ox and ox["median_s"] > 0:
                ratios.append(
                    {
                        "case": case,
                        "op": op,
                        "comparison": label,
                        "speed_ratio_poppler_over_oxide": pop["median_s"] / ox["median_s"],
                        "winner": "oxide" if ox["median_s"] < pop["median_s"] else "poppler",
                    }
                )
    return ratios


def write_markdown(results: dict, out_path: Path) -> None:
    rows = results["rows"]
    ratios = results["ratios"]
    lines = [
        "# Prompt H Performance Results",
        "",
        f"Generated: {results['generated_at']}",
        "",
        "## Context",
        "",
        f"- Platform: `{results['platform']}`",
        f"- CPU: `{results['cpu']}`",
        f"- Logical CPUs: {results['cpu_count']}",
        f"- Memory: {results['memory_mb']} MB",
        f"- Rust: `{results['rustc']}`",
        f"- Oxide: `{results['oxide_bin']}`",
        f"- Poppler: `{results['poppler_version'].splitlines()[0]}`",
        f"- DPI: {results['dpi']}",
        f"- Repeats: {results['repeats']} (median reported; first run is cold-start proxy)",
        "",
        "## Measurements",
        "",
        "| class | case | op | engine | median s | cold s | warm median s | peak MB | pages | rc |",
        "|---|---|---|---|---:|---:|---:|---:|---:|---|",
    ]
    for row in rows:
        warm = "" if row["warm_median_s"] is None else f"{row['warm_median_s']:.4f}"
        lines.append(
            "| {doc_class} | {case} | {op} | {engine_label} | {median:.4f} | "
            "{cold:.4f} | {warm} | {peak:.1f} | {pages} | {rcs} |".format(
                doc_class=row["doc_class"],
                case=row["case"],
                op=row["op"],
                engine_label=row["engine_label"],
                median=row["median_s"],
                cold=row["cold_s"],
                warm=warm,
                peak=row["peak_mb"] or 0.0,
                pages=row["pages"] if row["pages"] is not None else "",
                rcs=",".join(str(v) for v in row["rcs"]),
            )
        )
    lines.extend(
        [
            "",
            "## Speed Ratios",
            "",
            "`speed_ratio_poppler_over_oxide` is Poppler median time divided by Oxide median time; values above 1 mean Oxide is faster.",
            "",
            "| case | op | comparison | ratio | winner |",
            "|---|---|---|---:|---|",
        ]
    )
    for ratio in ratios:
        lines.append(
            "| {case} | {op} | {comparison} | {ratio:.3f} | {winner} |".format(
                case=ratio["case"],
                op=ratio["op"],
                comparison=ratio["comparison"],
                ratio=ratio["speed_ratio_poppler_over_oxide"],
                winner=ratio["winner"],
            )
        )
    out_path.write_text("\n".join(lines) + "\n", encoding="utf-8")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--poppler-bin-dir",
        default=str(REPO_ROOT / "target" / "tools" / "poppler" / "poppler-26.02.0" / "Library" / "bin"),
    )
    parser.add_argument("--oxide-bin", default=str(REPO_ROOT / "target" / "release" / exe("oxide")))
    parser.add_argument("--dpi", type=int, default=150)
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument("--threads", type=int, default=os.cpu_count() or 4)
    parser.add_argument("--timeout-sec", type=int, default=180)
    parser.add_argument("--cases", nargs="*")
    args = parser.parse_args()

    oxide_bin = Path(args.oxide_bin)
    if not oxide_bin.exists():
        raise SystemExit(f"missing Oxide release binary: {oxide_bin}")

    poppler_dir = Path(args.poppler_bin_dir)
    poppler = {
        "pdfinfo": poppler_dir / exe("pdfinfo"),
        "pdftotext": poppler_dir / exe("pdftotext"),
        "pdftoppm": poppler_dir / exe("pdftoppm"),
        "pdfimages": poppler_dir / exe("pdfimages"),
    }
    missing = [name for name, path in poppler.items() if not path.exists()]
    if missing:
        raise SystemExit(f"missing Poppler tools: {', '.join(missing)}")

    selected = [c for c in CASES if not args.cases or c.key in args.cases]
    if not selected:
        raise SystemExit(f"no matching cases; available: {[c.key for c in CASES]}")

    cpu = "unknown"
    memory_mb = None
    if IS_WINDOWS:
        try:
            cpu = subprocess.check_output(
                [
                    "powershell",
                    "-NoProfile",
                    "-Command",
                    "(Get-CimInstance Win32_Processor | Select-Object -First 1 -ExpandProperty Name)",
                ],
                text=True,
                timeout=10,
            ).strip()
            memory = subprocess.check_output(
                [
                    "powershell",
                    "-NoProfile",
                    "-Command",
                    "(Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory",
                ],
                text=True,
                timeout=10,
            ).strip()
            memory_mb = round(int(memory) / (1024 * 1024))
        except Exception:
            pass

    rows = []
    print(
        f"# Prompt H perf: repeats={args.repeats} dpi={args.dpi} threads=1,{args.threads}"
    )
    for case in selected:
        pdf = REPO_ROOT / case.path
        if not pdf.exists():
            print(f"missing {case.key}: {pdf}")
            continue
        pages = read_page_count(oxide_bin, pdf)
        for op in case.ops:
            engines: list[tuple[str, str, int | None]] = [
                ("oxide", "oxide@1", 1),
                ("oxide", "oxide@N", args.threads),
                ("poppler", "poppler", None),
            ]
            if op in ("info", "images"):
                engines = [("oxide", "oxide@1", 1), ("poppler", "poppler", None)]
            for engine, label, threads in engines:
                build = command_builder(engine, op, pdf, oxide_bin, poppler, args.dpi)
                measured = measure(build, threads, args.repeats, args.timeout_sec)
                row = {
                    "case": case.key,
                    "doc_class": case.doc_class,
                    "file": str(pdf),
                    "pages": pages,
                    "op": op,
                    "engine": engine,
                    "engine_label": label,
                    **measured,
                }
                rows.append(row)
                print(
                    f"{case.key:18} {op:13} {label:8} "
                    f"{row['median_s']:8.4f}s peak={row['peak_mb'] or 0:7.1f}MB rc={row['rcs']}"
                )

    results = {
        "generated_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "platform": platform.platform(),
        "cpu_count": os.cpu_count(),
        "cpu": cpu,
        "memory_mb": memory_mb,
        "rustc": version_text(["rustc", "--version"]),
        "cargo": version_text(["cargo", "--version"]),
        "oxide_bin": str(oxide_bin),
        "poppler_bin_dir": str(poppler_dir),
        "poppler_version": version_text([str(poppler["pdftoppm"]), "-v"]),
        "dpi": args.dpi,
        "repeats": args.repeats,
        "threads": [1, args.threads],
        "rows": rows,
    }
    results["ratios"] = summarize_ratios(rows)

    json_out = REPO_ROOT / "docs" / "perf_prompt_h_results.json"
    md_out = REPO_ROOT / "docs" / "perf_prompt_h_summary.md"
    json_out.write_text(json.dumps(results, indent=2), encoding="utf-8")
    write_markdown(results, md_out)
    print(f"wrote {json_out}")
    print(f"wrote {md_out}")


if __name__ == "__main__":
    main()
