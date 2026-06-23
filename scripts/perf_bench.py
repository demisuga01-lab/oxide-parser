#!/usr/bin/env python3
"""Oxide performance benchmark harness (Mega-Prompt 10, Part A).

Measures, for the release-build `oxide` CLI:
  * THROUGHPUT  — best-of-N wall-clock time per (command, file, thread-count).
  * PEAK MEMORY — maximum resident/working set during the run.

The parity harness (scripts/poppler_compare.py) measures correctness
(text similarity, render PSNR); it does NOT measure speed or memory. This
harness fills that gap so the parallel-text (Part B) and Arc-shared-render
(Part C) changes can show before/after deltas.

Why a script and not criterion: criterion measures throughput but not peak
memory, and the memory drop from the Arc fix is the headline metric here. A
single script that times the real CLI and samples peak memory keeps both
dimensions in one reproducible place, and is trivial to diff across rounds.

Peak memory:
  * Windows: Win32 GetProcessMemoryInfo -> PeakWorkingSetSize (an OS-maintained
    high-water mark, exact — not sampled). Read via ctypes, no dependencies.
  * Unix: /usr/bin/time -v "Maximum resident set size" when available, else a
    coarse RSS sampler thread.

Thread scaling: rayon honours RAYON_NUM_THREADS. We run each case at 1 thread
and at N threads (default: os.cpu_count()) so the parallelism win is explicit.
Pin threads => apples-to-apples before/after.

Usage (Windows, from repo root, after `cargo build --release`):
  py scripts/perf_bench.py --label before
  py scripts/perf_bench.py --label after
Outputs docs/perf_<label>_results.json and prints a markdown table.
"""

import argparse
import json
import os
import platform
import subprocess
import sys
import time
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
IS_WINDOWS = platform.system() == "Windows"

# ----------------------------------------------------------------------------
# Peak-memory measurement
# ----------------------------------------------------------------------------

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

    def run_and_measure(cmd, env):
        """Run cmd; return (elapsed_seconds, peak_bytes, returncode).

        Polls PeakWorkingSetSize (a monotonic OS high-water mark) while the
        process is alive, keeping the last successful reading. Because the
        counter is itself a peak, the final reading before exit IS the true
        peak — we don't have to catch the instantaneous maximum.
        """
        start = time.perf_counter()
        proc = subprocess.Popen(cmd, env=env, stdout=subprocess.DEVNULL,
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
        # Final reading right after exit may still succeed before the handle
        # is closed; capture it for the true high-water mark.
        if handle and _psapi.GetProcessMemoryInfo(
                handle, ctypes.byref(counters), counters.cb):
            peak = max(peak, counters.PeakWorkingSetSize)
        if handle:
            _kernel32.CloseHandle(handle)
        rc = proc.wait()
        elapsed = time.perf_counter() - start
        return elapsed, peak, rc

else:
    import threading
    import shutil

    def _sample_rss(pid, stop_evt, out):
        status = Path(f"/proc/{pid}/status")
        while not stop_evt.is_set():
            try:
                for line in status.read_text().splitlines():
                    if line.startswith("VmHWM:"):  # peak resident set size
                        kb = int(line.split()[1])
                        out[0] = max(out[0], kb * 1024)
            except (FileNotFoundError, ProcessLookupError, ValueError):
                pass
            time.sleep(0.002)

    def run_and_measure(cmd, env):
        start = time.perf_counter()
        proc = subprocess.Popen(cmd, env=env, stdout=subprocess.DEVNULL,
                                stderr=subprocess.DEVNULL)
        out = [0]
        stop = threading.Event()
        t = threading.Thread(target=_sample_rss, args=(proc.pid, stop, out))
        t.start()
        rc = proc.wait()
        stop.set()
        t.join()
        elapsed = time.perf_counter() - start
        # /proc VmHWM is itself a peak; final value is authoritative.
        try:
            for line in Path(f"/proc/{proc.pid}/status").read_text().splitlines():
                if line.startswith("VmHWM:"):
                    out[0] = max(out[0], int(line.split()[1]) * 1024)
        except Exception:
            pass
        return elapsed, out[0], rc


# ----------------------------------------------------------------------------
# Benchmark cases
# ----------------------------------------------------------------------------

def oxide_bin():
    name = "oxide.exe" if IS_WINDOWS else "oxide"
    p = REPO_ROOT / "target" / "release" / name
    if not p.exists():
        sys.exit(f"release binary not found: {p}\n"
                 f"build it first: cargo build --release -p oxide-cli")
    return str(p)


# (key, fixture-relative-path) — large multi-page docs are the stress cases.
FIXTURE_DIR = REPO_ROOT / "crates" / "engine" / "tests" / "fixtures"
CORPUS_GEN = REPO_ROOT / "tests" / "corpus" / "pdfs" / "generated"

CASES = [
    ("120pg", CORPUS_GEN / "generated_120_pages.pdf"),
    ("tracemonkey", FIXTURE_DIR / "tracemonkey.pdf"),
    ("form_160f", FIXTURE_DIR / "form_160f.pdf"),
]


def mb(n):
    return n / (1024 * 1024)


def bench_one(binary, subcmd, pdf, extra_args, threads, repeats, temp_root):
    """Best-of-N time, max peak across runs."""
    env = dict(os.environ)
    env["RAYON_NUM_THREADS"] = str(threads)
    best_time = None
    peak = 0
    rc_final = 0
    temp_root.mkdir(parents=True, exist_ok=True)
    # Python-created temp subdirectories can be denied to child processes in the
    # Windows restricted-token sandbox. Write each run directly under target/.
    out = temp_root / f"{subcmd}-{threads}-{os.getpid()}-{time.perf_counter_ns()}.bin"
    cmd = [binary, subcmd, str(pdf)] + extra_args
    if subcmd in ("render",):
        cmd += ["--output", str(out)]
    elif subcmd == "extract-text":
        cmd += ["--output", str(out)]
    for _ in range(repeats):
        elapsed, pk, rc = run_and_measure(cmd, env)
        rc_final = rc
        if best_time is None or elapsed < best_time:
            best_time = elapsed
        peak = max(peak, pk)
    return best_time, peak, rc_final


def main():
    ap = argparse.ArgumentParser(description="Oxide perf benchmark")
    ap.add_argument("--label", default="run",
                    help="label for output file (e.g. before/after)")
    ap.add_argument("--repeats", type=int, default=5,
                    help="runs per case; report best time, max peak")
    ap.add_argument("--dpi", type=int, default=150)
    ap.add_argument("--max-threads", type=int, default=os.cpu_count() or 4)
    ap.add_argument("--cases", nargs="*",
                    help="subset of case keys to run (default: all)")
    ap.add_argument("--temp-root", type=Path,
                    default=REPO_ROOT / "target" / "perf-bench-tmp",
                    help="workspace temp root for per-run output files")
    args = ap.parse_args()

    binary = oxide_bin()
    thread_counts = sorted({1, args.max_threads})

    results = {
        "label": args.label,
        "platform": platform.platform(),
        "cpu_count": os.cpu_count(),
        "dpi": args.dpi,
        "repeats": args.repeats,
        "binary": binary,
        "cases": [],
    }

    selected = [c for c in CASES if not args.cases or c[0] in args.cases]
    if not selected:
        sys.exit(f"no matching cases; available: {[c[0] for c in CASES]}")

    print(f"# Oxide perf benchmark — label={args.label}")
    print(f"platform={platform.platform()} cpus={os.cpu_count()} "
          f"dpi={args.dpi} repeats={args.repeats} (best time, max peak)\n")
    header = (f"{'case':<14}{'op':<14}{'threads':>8}"
              f"{'time_s':>10}{'peak_MB':>10}{'rc':>4}")
    print(header)
    print("-" * len(header))

    for key, pdf in selected:
        if not pdf.exists():
            print(f"{key:<14}MISSING: {pdf}")
            continue
        for threads in thread_counts:
            # Text extraction.
            t, pk, rc = bench_one(binary, "extract-text", pdf, [],
                                  threads, args.repeats, args.temp_root)
            results["cases"].append({
                "case": key, "op": "extract-text", "threads": threads,
                "time_s": t, "peak_bytes": pk, "rc": rc, "file": str(pdf),
            })
            print(f"{key:<14}{'extract-text':<14}{threads:>8}"
                  f"{t:>10.3f}{mb(pk):>10.1f}{rc:>4}")
            # Render.
            t, pk, rc = bench_one(binary, "render", pdf,
                                  ["--dpi", str(args.dpi), "--format", "png"],
                                  threads, args.repeats, args.temp_root)
            results["cases"].append({
                "case": key, "op": "render", "threads": threads,
                "time_s": t, "peak_bytes": pk, "rc": rc, "file": str(pdf),
            })
            print(f"{key:<14}{'render':<14}{threads:>8}"
                  f"{t:>10.3f}{mb(pk):>10.1f}{rc:>4}")

    out_path = REPO_ROOT / "docs" / f"perf_{args.label}_results.json"
    out_path.write_text(json.dumps(results, indent=2))
    print(f"\nwrote {out_path}")


if __name__ == "__main__":
    main()
