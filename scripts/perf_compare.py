#!/usr/bin/env python3
"""Oxide-vs-Poppler performance comparison (Mega-Prompt 15, Part B).

The parity harness (scripts/poppler_compare.py) measures *correctness*
(text similarity, render PSNR). scripts/perf_bench.py measures Oxide's own
1-vs-N-thread speed and peak memory but does NOT compare against Poppler.
This harness fills that gap: it times the SAME operations on the SAME inputs
for both engines and records best-of-N wall-clock time and peak memory.

Operations compared (same input, same DPI):
  * text   : oxide extract-text   vs poppler pdftotext
  * render : oxide render @150dpi  vs poppler pdftoppm -r 150  (ALL pages)
  * images : oxide extract-images  vs poppler pdfimages

Threads: Oxide honours RAYON_NUM_THREADS; we run it at 1 thread AND at N
threads so the per-core comparison (Oxide@1 vs Poppler, both single-threaded)
and the multi-thread advantage (Oxide@N vs Poppler) are both visible and
honest. Poppler's CLI tools are single-threaded, so Poppler is run once.

Peak memory: Windows GetProcessMemoryInfo.PeakWorkingSetSize (OS high-water
mark, exact); Unix /proc VmHWM. Same sampler as perf_bench.py.

NOTE: outputs are not byte-identical between engines (oxide render = PNG-in-ZIP,
pdftoppm = PPM files; oxide extract-images = ZIP, pdfimages = loose files), so
these are wall-clock/throughput comparisons of equivalent work, not of
identical artifacts. Correctness/parity is measured separately by the parity
harness. Use RELEASE builds.

Usage (from repo root, after `cargo build --release`):
  py scripts/perf_compare.py --poppler-bin-dir target/tools/poppler/poppler-26.02.0/Library/bin
Outputs docs/perf_compare_results.json and prints a markdown table.
"""

import argparse
import json
import os
import platform
import subprocess
import sys
import tempfile
import time
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
IS_WINDOWS = platform.system() == "Windows"

# ---------------------------------------------------------------------------
# Peak-memory measurement (identical approach to perf_bench.py)
# ---------------------------------------------------------------------------
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

    def run_and_measure(cmd, env, cwd):
        start = time.perf_counter()
        proc = subprocess.Popen(cmd, env=env, cwd=cwd,
                                stdout=subprocess.DEVNULL,
                                stderr=subprocess.DEVNULL)
        handle = _kernel32.OpenProcess(
            PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, False, proc.pid)
        peak = 0
        counters = PROCESS_MEMORY_COUNTERS()
        counters.cb = ctypes.sizeof(PROCESS_MEMORY_COUNTERS)
        while proc.poll() is None:
            if handle and _psapi.GetProcessMemoryInfo(
                    handle, ctypes.byref(counters), counters.cb):
                peak = max(peak, counters.PeakWorkingSetSize)
            time.sleep(0.002)
        if handle and _psapi.GetProcessMemoryInfo(
                handle, ctypes.byref(counters), counters.cb):
            peak = max(peak, counters.PeakWorkingSetSize)
        if handle:
            _kernel32.CloseHandle(handle)
        rc = proc.wait()
        return time.perf_counter() - start, peak, rc
else:
    import threading

    def _sample_rss(pid, stop_evt, out):
        status = Path(f"/proc/{pid}/status")
        while not stop_evt.is_set():
            try:
                for line in status.read_text().splitlines():
                    if line.startswith("VmHWM:"):
                        out[0] = max(out[0], int(line.split()[1]) * 1024)
            except (FileNotFoundError, ProcessLookupError, ValueError):
                pass
            time.sleep(0.002)

    def run_and_measure(cmd, env, cwd):
        start = time.perf_counter()
        proc = subprocess.Popen(cmd, env=env, cwd=cwd,
                                stdout=subprocess.DEVNULL,
                                stderr=subprocess.DEVNULL)
        out = [0]
        stop = threading.Event()
        t = threading.Thread(target=_sample_rss, args=(proc.pid, stop, out))
        t.start()
        rc = proc.wait()
        stop.set()
        t.join()
        return time.perf_counter() - start, out[0], rc


def exe(name):
    return name + ".exe" if IS_WINDOWS and not name.endswith(".exe") else name


def mb(n):
    return n / (1024 * 1024)


# (key, path-relative-to-repo, [ops]) — ops drawn from {text, render, images}
CASES = [
    ("small_text", "tests/corpus/pdfs/generated/generated_basic_text.pdf",
     ["text", "render"]),
    ("tracemonkey", "crates/engine/tests/fixtures/tracemonkey.pdf",
     ["text", "render"]),
    ("form_160f", "crates/engine/tests/fixtures/form_160f.pdf",
     ["text", "render"]),
    ("large_120pg", "tests/corpus/pdfs/generated/generated_120_pages.pdf",
     ["text", "render"]),
    ("images_1.5mb", "tests/corpus/pdfs/pdfjs/images.pdf",
     ["render", "images"]),
]


def best_of(builder, env_threads, cwd, repeats):
    """Run a command `repeats` times; return (best_time, max_peak, rc)."""
    env = dict(os.environ)
    if env_threads is not None:
        env["RAYON_NUM_THREADS"] = str(env_threads)
    best_t, peak, rc = None, 0, 0
    with tempfile.TemporaryDirectory() as td:
        for _ in range(repeats):
            cmd = builder(Path(td))
            t, pk, rc = run_and_measure(cmd, env, cwd)
            best_t = t if best_t is None else min(best_t, t)
            peak = max(peak, pk)
    return best_t, peak, rc


def main():
    ap = argparse.ArgumentParser(description="Oxide vs Poppler perf comparison")
    ap.add_argument("--poppler-bin-dir", required=True)
    ap.add_argument("--oxide-bin",
                    default=str(REPO_ROOT / "target" / "release" / exe("oxide")))
    ap.add_argument("--repeats", type=int, default=3)
    ap.add_argument("--dpi", type=int, default=150)
    ap.add_argument("--max-threads", type=int, default=os.cpu_count() or 4)
    ap.add_argument("--cases", nargs="*")
    args = ap.parse_args()

    oxide = Path(args.oxide_bin)
    if not oxide.exists():
        sys.exit(f"oxide binary not found: {oxide} (build with cargo build --release)")
    pbin = Path(args.poppler_bin_dir).resolve()
    pdftotext = pbin / exe("pdftotext")
    pdftoppm = pbin / exe("pdftoppm")
    pdfimages = pbin / exe("pdfimages")
    for tool in (pdftotext, pdftoppm, pdfimages):
        if not tool.exists():
            sys.exit(f"poppler tool not found: {tool}")

    n = args.max_threads
    results = {
        "platform": platform.platform(),
        "cpu_count": os.cpu_count(),
        "dpi": args.dpi,
        "repeats": args.repeats,
        "n_threads": n,
        "oxide_bin": str(oxide),
        "poppler_bin_dir": str(pbin),
        "rows": [],
    }

    def add(case, op, engine, threads, t, peak, rc, pages=None):
        results["rows"].append({
            "case": case, "op": op, "engine": engine, "threads": threads,
            "time_s": t, "peak_bytes": peak, "rc": rc, "pages": pages,
        })

    print(f"# Oxide vs Poppler — platform={platform.platform()} "
          f"cpus={os.cpu_count()} dpi={args.dpi} repeats={args.repeats}\n")
    hdr = f"{'case':<14}{'op':<9}{'engine':<16}{'thr':>4}{'time_s':>10}{'peak_MB':>10}{'rc':>4}"
    print(hdr)
    print("-" * len(hdr))

    def emit(case, op, engine, threads, t, peak, rc, pages=None):
        add(case, op, engine, threads, t, peak, rc, pages)
        label = f"oxide@{threads}" if engine == "oxide" else "poppler"
        print(f"{case:<14}{op:<9}{label:<16}{threads:>4}{t:>10.3f}{mb(peak):>10.1f}{rc:>4}")

    selected = [c for c in CASES if not args.cases or c[0] in args.cases]
    for key, rel, ops in selected:
        pdf = REPO_ROOT / rel
        if not pdf.exists():
            print(f"{key:<14}MISSING {pdf}")
            continue
        for op in ops:
            if op == "text":
                ot, opk, orc = best_of(
                    lambda td: [str(oxide), "extract-text", str(pdf),
                                "--output", str(td / "o.txt")],
                    1, str(REPO_ROOT), args.repeats)
                emit(key, op, "oxide", 1, ot, opk, orc)
                otn, opkn, orcn = best_of(
                    lambda td: [str(oxide), "extract-text", str(pdf),
                                "--output", str(td / "o.txt")],
                    n, str(REPO_ROOT), args.repeats)
                emit(key, op, "oxide", n, otn, opkn, orcn)
                pt, ppk, prc = best_of(
                    lambda td: [str(pdftotext), "-enc", "UTF-8", "-nopgbrk",
                                str(pdf), str(td / "p.txt")],
                    None, str(pbin), args.repeats)
                emit(key, op, "poppler", 1, pt, ppk, prc)
            elif op == "render":
                ot, opk, orc = best_of(
                    lambda td: [str(oxide), "render", str(pdf),
                                "--output", str(td / "o.zip"),
                                "--dpi", str(args.dpi), "--format", "png"],
                    1, str(REPO_ROOT), args.repeats)
                emit(key, op, "oxide", 1, ot, opk, orc)
                otn, opkn, orcn = best_of(
                    lambda td: [str(oxide), "render", str(pdf),
                                "--output", str(td / "o.zip"),
                                "--dpi", str(args.dpi), "--format", "png"],
                    n, str(REPO_ROOT), args.repeats)
                emit(key, op, "oxide", n, otn, opkn, orcn)
                pt, ppk, prc = best_of(
                    lambda td: [str(pdftoppm), "-r", str(args.dpi),
                                str(pdf), str(td / "pg")],
                    None, str(pbin), args.repeats)
                emit(key, op, "poppler", 1, pt, ppk, prc)
            elif op == "images":
                ot, opk, orc = best_of(
                    lambda td: [str(oxide), "extract-images", str(pdf),
                                "--output", str(td / "o.zip"),
                                "--format", "original"],
                    1, str(REPO_ROOT), args.repeats)
                emit(key, op, "oxide", 1, ot, opk, orc)
                pt, ppk, prc = best_of(
                    lambda td: [str(pdfimages), str(pdf), str(td / "im")],
                    None, str(pbin), args.repeats)
                emit(key, op, "poppler", 1, pt, ppk, prc)

    out = REPO_ROOT / "docs" / "perf_compare_results.json"
    out.write_text(json.dumps(results, indent=2))
    print(f"\nwrote {out}")


if __name__ == "__main__":
    main()
