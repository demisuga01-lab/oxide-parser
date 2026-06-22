#!/usr/bin/env python3
"""Capstone SDK operation benchmark.

Measures representative release-build operations and writes
docs/capstone_sdk_operation_benchmarks.json. This is intentionally a small,
repeatable smoke benchmark, not a renderer-fidelity suite.
"""

from __future__ import annotations

import argparse
import ctypes
import json
import os
import platform
import subprocess
import sys
import time
from ctypes import wintypes
from pathlib import Path


REPO = Path(__file__).resolve().parents[1]
IS_WINDOWS = os.name == "nt"
EXE = ".exe" if IS_WINDOWS else ""
RELEASE = REPO / "target" / "release"
ENGINE_FIXTURES = REPO / "crates" / "engine" / "tests" / "fixtures"
OUT = REPO / "docs" / "capstone_sdk_operation_benchmarks.json"


if IS_WINDOWS:

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

    PSAPI = ctypes.WinDLL("psapi", use_last_error=True)
    KERNEL32 = ctypes.WinDLL("kernel32", use_last_error=True)
    PROCESS_QUERY_INFORMATION = 0x0400
    PROCESS_VM_READ = 0x0010


def run_and_measure(cmd: list[str], *, env: dict[str, str] | None = None, timeout: int = 120) -> dict:
    start = time.perf_counter()
    proc = subprocess.Popen(
        cmd,
        cwd=REPO,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    peak = 0
    handle = None
    if IS_WINDOWS:
        handle = KERNEL32.OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, False, proc.pid)
        counters = PROCESS_MEMORY_COUNTERS()
        counters.cb = ctypes.sizeof(PROCESS_MEMORY_COUNTERS)
    try:
        while proc.poll() is None:
            if time.perf_counter() - start > timeout:
                proc.kill()
                out, err = proc.communicate()
                return {
                    "ok": False,
                    "exit_code": None,
                    "elapsed_ms": round((time.perf_counter() - start) * 1000, 1),
                    "peak_mb": round(peak / 1048576, 2),
                    "timed_out": True,
                    "stdout": out[-1000:],
                    "stderr": err[-1000:],
                }
            if IS_WINDOWS and handle and PSAPI.GetProcessMemoryInfo(handle, ctypes.byref(counters), counters.cb):
                peak = max(peak, counters.PeakWorkingSetSize)
            time.sleep(0.003)
        out, err = proc.communicate()
        if IS_WINDOWS and handle and PSAPI.GetProcessMemoryInfo(handle, ctypes.byref(counters), counters.cb):
            peak = max(peak, counters.PeakWorkingSetSize)
        return {
            "ok": proc.returncode == 0,
            "exit_code": proc.returncode,
            "elapsed_ms": round((time.perf_counter() - start) * 1000, 1),
            "peak_mb": round(peak / 1048576, 2) if peak else None,
            "timed_out": False,
            "stdout": out[-1000:],
            "stderr": err[-1000:],
        }
    finally:
        if IS_WINDOWS and handle:
            KERNEL32.CloseHandle(handle)


def best_of(cmd: list[str], repeats: int, label: str) -> dict:
    runs = [run_and_measure(cmd) for _ in range(repeats)]
    successful = [r for r in runs if r["ok"]]
    best = min(successful or runs, key=lambda r: r["elapsed_ms"])
    return {
        "label": label,
        "command": cmd,
        "repeats": repeats,
        "best_elapsed_ms": best["elapsed_ms"],
        "max_peak_mb": max((r["peak_mb"] or 0 for r in runs), default=0),
        "all_ok": all(r["ok"] for r in runs),
        "runs": runs,
    }


def version(cmd: list[str]) -> str:
    try:
        proc = subprocess.run(cmd, cwd=REPO, capture_output=True, text=True, timeout=10)
        return (proc.stdout + proc.stderr).strip()
    except Exception as exc:  # noqa: BLE001
        return str(exc)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repeats", type=int, default=3)
    args = parser.parse_args()

    oxide = RELEASE / f"oxide{EXE}"
    authoring = RELEASE / "examples" / f"authoring{EXE}"
    compliance = RELEASE / "examples" / f"compliance{EXE}"
    sign_document = RELEASE / "examples" / f"sign_document{EXE}"
    required = [oxide, authoring, compliance, sign_document]
    missing = [str(path) for path in required if not path.exists()]
    if missing:
        print(f"missing release benchmark binary: {missing}", file=sys.stderr)
        return 1

    out_root = REPO / "target" / "capstone-op-bench"
    out_root.mkdir(parents=True, exist_ok=True)
    basic = ENGINE_FIXTURES / "basicapi.pdf"
    multi = ENGINE_FIXTURES / "multi_stream.pdf"
    key = ENGINE_FIXTURES / "sign_test_rsa_key.pem"
    cert = ENGINE_FIXTURES / "sign_test_rsa_cert.pem"

    operations = [
        (
            "parse_json_cli",
            [str(oxide), "parse", str(basic), "--format", "json", "--output", str(out_root / "parse.json")],
        ),
        (
            "extract_text_cli",
            [str(oxide), "extract-text", str(basic), "--output", str(out_root / "text.txt")],
        ),
        (
            "render_png_cli",
            [
                str(oxide),
                "render",
                str(basic),
                "--format",
                "png",
                "--dpi",
                "100",
                "--output",
                str(out_root / "render.zip"),
            ],
        ),
        ("authoring_example", [str(authoring), str(out_root / "authored.pdf")]),
        ("pdfa_conversion_example", [str(compliance), str(out_root / "compliance")]),
        (
            "sign_rsa_example",
            [
                str(sign_document),
                str(basic),
                str(key),
                str(cert),
                str(out_root / "signed.pdf"),
            ],
        ),
        (
            "optimize_cli",
            [str(oxide), "optimize", str(multi), "--output", str(out_root / "optimized.pdf"), "--json"],
        ),
        (
            "linearize_cli",
            [str(oxide), "linearize", str(basic), "--output", str(out_root / "linearized.pdf")],
        ),
        (
            "encrypt_aes256_cli",
            [
                str(oxide),
                "encrypt",
                str(basic),
                "--output",
                str(out_root / "encrypted.pdf"),
                "--user-pw",
                "capstone",
                "--owner-pw",
                "owner",
                "--algo",
                "aes256",
                "--json",
            ],
        ),
    ]

    results = {
        "commit": version(["git", "rev-parse", "--short", "HEAD"]),
        "dirty": bool(version(["git", "status", "--short"])),
        "platform": platform.platform(),
        "cpu_count": os.cpu_count(),
        "python": sys.version.split()[0],
        "tool_versions": {
            "oxide": version([str(oxide), "--version"]),
            "qpdf": version(["qpdf", "--version"]),
            "pdftoppm": version(["pdftoppm", "-v"]),
            "verapdf": version([str(REPO / "target" / "tools" / "verapdf" / "app" / "verapdf.bat"), "--version"]),
        },
        "operations": [],
    }
    for label, cmd in operations:
        print(f"benchmarking {label}")
        results["operations"].append(best_of(cmd, args.repeats, label))

    OUT.write_text(json.dumps(results, indent=2) + "\n", encoding="utf-8")
    print(f"wrote {OUT}")
    return 0 if all(op["all_ok"] for op in results["operations"]) else 1


if __name__ == "__main__":
    raise SystemExit(main())
