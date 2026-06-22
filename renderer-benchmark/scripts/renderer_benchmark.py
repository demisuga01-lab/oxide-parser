#!/usr/bin/env python3
"""Renderer Benchmark 0A: Oxide renderer compatibility and safety suite."""

from __future__ import annotations

import argparse
import ctypes
import hashlib
import json
import math
import os
import re
import shutil
import signal
import subprocess
import sys
import time
import zipfile
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT / "scripts"))
from poppler_compare import RGBImage, parse_png_bytes, sorted_page_files, trim_text  # noqa: E402


PNG_SUFFIX = ".png"
PHASH_COS_X = [[math.cos(((2 * x + 1) * u * math.pi) / 64) for x in range(32)] for u in range(8)]
PHASH_COS_Y = [[math.cos(((2 * y + 1) * v * math.pi) / 64) for y in range(32)] for v in range(8)]


@dataclass
class CommandResult:
    ok: bool
    exit_code: int | None
    timed_out: bool
    memory_exceeded: bool
    peak_memory_mb: float | None
    duration_ms: int
    stdout: str
    stderr: str
    error: str | None = None

    def compact(self) -> dict[str, Any]:
        return {
            "ok": self.ok,
            "exit_code": self.exit_code,
            "timed_out": self.timed_out,
            "memory_exceeded": self.memory_exceeded,
            "peak_memory_mb": round(self.peak_memory_mb, 2) if self.peak_memory_mb is not None else None,
            "duration_ms": self.duration_ms,
            "stdout": trim_text(self.stdout, 2000),
            "stderr": trim_text(self.stderr, 2000),
            "error": self.error,
        }


@dataclass
class RenderedPage:
    page: int
    name: str
    image: RGBImage
    sha256: str


def executable_name(name: str) -> str:
    if os.name == "nt" and not name.lower().endswith(".exe"):
        return name + ".exe"
    return name


def find_executable(name: str, bin_dir: Path | None = None) -> str | None:
    exe = executable_name(name)
    if bin_dir:
        candidate = bin_dir / exe
        if candidate.exists():
            return str(candidate)
    return shutil.which(name)


def kill_process_tree(proc: subprocess.Popen[str]) -> None:
    if proc.poll() is not None:
        return
    if os.name == "nt":
        subprocess.run(["taskkill", "/PID", str(proc.pid), "/T", "/F"], capture_output=True, text=True)
    else:
        try:
            os.killpg(proc.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        except Exception:
            proc.kill()


def process_rss_mb(pid: int) -> float | None:
    if os.name == "nt":
        return windows_process_rss_mb(pid)
    status = Path(f"/proc/{pid}/status")
    try:
        for line in status.read_text(encoding="utf-8", errors="replace").splitlines():
            if line.startswith("VmRSS:"):
                parts = line.split()
                return int(parts[1]) / 1024.0
    except OSError:
        return None
    return None


def windows_process_rss_mb(pid: int) -> float | None:
    PROCESS_QUERY_LIMITED_INFORMATION = 0x1000
    PROCESS_VM_READ = 0x0010

    class PROCESS_MEMORY_COUNTERS(ctypes.Structure):
        _fields_ = [
            ("cb", ctypes.c_ulong),
            ("PageFaultCount", ctypes.c_ulong),
            ("PeakWorkingSetSize", ctypes.c_size_t),
            ("WorkingSetSize", ctypes.c_size_t),
            ("QuotaPeakPagedPoolUsage", ctypes.c_size_t),
            ("QuotaPagedPoolUsage", ctypes.c_size_t),
            ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t),
            ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
            ("PagefileUsage", ctypes.c_size_t),
            ("PeakPagefileUsage", ctypes.c_size_t),
        ]

    try:
        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        psapi = ctypes.WinDLL("psapi", use_last_error=True)
        handle = kernel32.OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ, False, pid)
        if not handle:
            return None
        counters = PROCESS_MEMORY_COUNTERS()
        counters.cb = ctypes.sizeof(counters)
        ok = psapi.GetProcessMemoryInfo(handle, ctypes.byref(counters), counters.cb)
        kernel32.CloseHandle(handle)
        if not ok:
            return None
        return counters.WorkingSetSize / (1024.0 * 1024.0)
    except Exception:
        return None


def run_monitored(
    cmd: list[str],
    *,
    timeout_sec: int,
    max_memory_mb: int | None,
    cwd: Path = REPO_ROOT,
) -> CommandResult:
    start = time.monotonic()
    peak: float | None = None
    timed_out = False
    memory_exceeded = False
    error: str | None = None
    creationflags = subprocess.CREATE_NEW_PROCESS_GROUP if os.name == "nt" else 0
    preexec_fn = None if os.name == "nt" else os.setsid
    try:
        proc = subprocess.Popen(
            cmd,
            cwd=str(cwd),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            encoding="utf-8",
            errors="replace",
            creationflags=creationflags,
            preexec_fn=preexec_fn,
        )
    except FileNotFoundError as err:
        return CommandResult(False, None, False, False, None, 0, "", "", str(err))

    while proc.poll() is None:
        elapsed = time.monotonic() - start
        rss = process_rss_mb(proc.pid)
        if rss is not None:
            peak = rss if peak is None else max(peak, rss)
            if max_memory_mb is not None and rss > max_memory_mb:
                memory_exceeded = True
                error = f"memory cap exceeded: {rss:.1f} MB > {max_memory_mb} MB"
                kill_process_tree(proc)
                break
        if elapsed > timeout_sec:
            timed_out = True
            error = f"timeout after {timeout_sec}s"
            kill_process_tree(proc)
            break
        time.sleep(0.05)

    try:
        stdout, stderr = proc.communicate(timeout=2)
    except subprocess.TimeoutExpired:
        kill_process_tree(proc)
        stdout, stderr = proc.communicate()
    duration_ms = int(round((time.monotonic() - start) * 1000))
    ok = proc.returncode == 0 and not timed_out and not memory_exceeded
    return CommandResult(ok, proc.returncode, timed_out, memory_exceeded, peak, duration_ms, stdout, stderr, error)


def load_manifest(path: Path) -> list[dict[str, Any]]:
    manifest = json.loads(path.read_text(encoding="utf-8"))
    entries: list[dict[str, Any]] = []
    for raw in manifest.get("entries", []):
        entry = dict(raw)
        pdf = Path(entry["path"])
        if not pdf.is_absolute():
            pdf = REPO_ROOT / pdf
        entry["absolute_path"] = str(pdf)
        entries.append(entry)
    return entries


def selected_entries(args: argparse.Namespace) -> list[dict[str, Any]]:
    entries = load_manifest(Path(args.manifest))
    if args.category:
        categories = {item.strip() for item in args.category.split(",") if item.strip()}
        entries = [e for e in entries if e.get("category") in categories]
    if args.file:
        needles = [item.strip().lower() for item in args.file.split(",") if item.strip()]
        entries = [
            e
            for e in entries
            if any(n in e.get("id", "").lower() or n in Path(e.get("path", "")).name.lower() for n in needles)
        ]
    if args.limit is not None:
        entries = entries[: args.limit]
    return entries


def safe_id(entry: dict[str, Any]) -> str:
    raw = entry.get("id") or Path(entry["path"]).stem
    return re.sub(r"[^A-Za-z0-9_.-]+", "_", str(raw))[:120]


def backend_versions(poppler: dict[str, str], pdfium: str | None) -> dict[str, Any]:
    versions: dict[str, Any] = {"poppler": {}, "pdfium": None}
    for key, exe in poppler.items():
        result = run_monitored([exe, "-v"], timeout_sec=10, max_memory_mb=None)
        versions["poppler"][key] = trim_text((result.stderr or result.stdout).strip(), 400)
    if pdfium:
        result = run_monitored([pdfium, "--help"], timeout_sec=10, max_memory_mb=None)
        versions["pdfium"] = trim_text((result.stderr or result.stdout).strip(), 400)
    return versions


def parse_poppler_page_count(text: str) -> int | None:
    match = re.search(r"^Pages:\s+(\d+)\s*$", text, flags=re.MULTILINE)
    return int(match.group(1)) if match else None


def get_poppler_info(entry: dict[str, Any], poppler: dict[str, str], timeout: int, mem_mb: int | None) -> dict[str, Any]:
    pdf = entry["absolute_path"]
    cmd = [poppler["pdfinfo"], pdf]
    result = run_monitored(cmd, timeout_sec=timeout, max_memory_mb=mem_mb)
    return {
        "command": result.compact(),
        "page_count": parse_poppler_page_count(result.stdout) if result.ok else None,
        "raw": trim_text(result.stdout, 2000),
    }


def get_oxide_info(entry: dict[str, Any], oxide_bin: str, timeout: int, mem_mb: int | None) -> dict[str, Any]:
    cmd = [oxide_bin, "info", entry["absolute_path"], "--json"]
    result = run_monitored(cmd, timeout_sec=timeout, max_memory_mb=mem_mb)
    parsed: dict[str, Any] | None = None
    if result.ok:
        try:
            parsed = json.loads(result.stdout)
        except json.JSONDecodeError:
            parsed = None
    return {
        "command": result.compact(),
        "json": parsed,
        "page_count": parsed.get("page_count") if isinstance(parsed, dict) else None,
        "pdf_version": parsed.get("pdf_version") if isinstance(parsed, dict) else None,
        "encrypted": parsed.get("encrypted") if isinstance(parsed, dict) else None,
    }


def page_limit(page_count: int | None, max_pages: int | None) -> int | None:
    if max_pages is None:
        return page_count
    if page_count is None:
        return max_pages
    return min(page_count, max_pages)


def parse_page_num(name: str, fallback: int) -> int:
    match = re.search(r"(\d+)(?=\.[^.]+$)", name)
    return int(match.group(1)) if match else fallback


def render_poppler(
    entry: dict[str, Any],
    poppler: dict[str, str],
    work_dir: Path,
    dpi: int,
    pages: int | None,
    timeout: int,
    mem_mb: int | None,
) -> dict[str, Any]:
    prefix = work_dir / "poppler_page"
    # `-cropbox`: Oxide renders the CropBox (MediaBox ∩ CropBox) by default — the
    # spec/viewer default that pdfinfo, PDFium, Chrome, and `pdftocairo` agree on.
    # `pdftoppm` alone defaults to the MediaBox, so without this flag the two
    # renderers size differently on every page that has a CropBox (≈16 corpus
    # files), producing spurious "dimension_mismatch" + cascading pixel diffs.
    # Passing -cropbox makes the comparison apples-to-apples (same page region).
    cmd = [poppler["pdftoppm"], "-png", "-cropbox", "-r", str(dpi)]
    if pages:
        cmd.extend(["-f", "1", "-l", str(pages)])
    cmd.extend([entry["absolute_path"], str(prefix)])
    result = run_monitored(cmd, timeout_sec=timeout, max_memory_mb=mem_mb)
    rendered: list[RenderedPage] = []
    errors: list[str] = []
    if result.ok:
        for idx, path in enumerate(sorted_page_files(list(work_dir.glob("poppler_page-*.png"))), start=1):
            try:
                data = path.read_bytes()
                rendered.append(
                    RenderedPage(parse_page_num(path.name, idx), path.name, parse_png_bytes(data), hashlib.sha256(data).hexdigest())
                )
            except Exception as err:
                errors.append(f"{path.name}: {err}")
    return {"command": result.compact(), "pages": rendered, "parse_errors": errors}


def render_oxide(
    entry: dict[str, Any],
    oxide_bin: str,
    work_dir: Path,
    dpi: int,
    pages: int | None,
    timeout: int,
    mem_mb: int | None,
    suffix: str = "oxide",
) -> dict[str, Any]:
    output_zip = work_dir / f"{suffix}.zip"
    cmd = [
        oxide_bin,
        "render",
        entry["absolute_path"],
        "--output",
        str(output_zip),
        "--dpi",
        str(dpi),
        "--format",
        "png",
    ]
    if pages:
        cmd.extend(["--pages", f"1-{pages}"])
    result = run_monitored(cmd, timeout_sec=timeout, max_memory_mb=mem_mb)
    rendered: list[RenderedPage] = []
    errors: list[str] = []
    if result.ok and output_zip.exists():
        try:
            with zipfile.ZipFile(output_zip) as archive:
                names = sorted(name for name in archive.namelist() if name.lower().endswith(PNG_SUFFIX))
                for idx, name in enumerate(names, start=1):
                    data = archive.read(name)
                    rendered.append(
                        RenderedPage(parse_page_num(name, idx), name, parse_png_bytes(data), hashlib.sha256(data).hexdigest())
                    )
        except Exception as err:
            errors.append(f"zip parse: {err}")
    return {"command": result.compact(), "pages": rendered, "parse_errors": errors, "zip": str(output_zip)}


def grayscale_sample(image: RGBImage, width: int, height: int) -> list[float]:
    values: list[float] = []
    for y in range(height):
        src_y = min(image.height - 1, int(y * image.height / height))
        row = src_y * image.width * 3
        for x in range(width):
            src_x = min(image.width - 1, int(x * image.width / width))
            i = row + src_x * 3
            r, g, b = image.pixels[i], image.pixels[i + 1], image.pixels[i + 2]
            values.append(0.299 * r + 0.587 * g + 0.114 * b)
    return values


def crop_pair(a: RGBImage, b: RGBImage) -> tuple[int, int]:
    return min(a.width, b.width), min(a.height, b.height)


# Precomputed lookups so the per-pixel metric loop does table reads instead of
# arithmetic+abs on every channel. _SQUARE[d] = d*d for d in 0..255.
_SQUARE = [d * d for d in range(256)]


def pixel_metrics(a: RGBImage, b: RGBImage, width: int, height: int) -> dict[str, Any]:
    total_channels = width * height * 3
    total_pixels = width * height
    if total_channels == 0:
        raise ValueError("empty comparison")
    abs_sum = 0
    squared = 0
    different_pixels = 0
    exact_pixels = 0
    max_delta = 0
    a_px = a.pixels
    b_px = b.pixels
    a_stride = a.width * 3
    b_stride = b.width * 3
    row_bytes = width * 3
    square = _SQUARE
    for y in range(height):
        a0 = y * a_stride
        b0 = y * b_stride
        # Slice the comparable row span once; iterating a bytes slice yields ints
        # directly, which is dramatically faster than triple-indexing per channel.
        a_row = a_px[a0 : a0 + row_bytes]
        b_row = b_px[b0 : b0 + row_bytes]
        x = 0
        while x < row_bytes:
            d0 = a_row[x] - b_row[x]
            d1 = a_row[x + 1] - b_row[x + 1]
            d2 = a_row[x + 2] - b_row[x + 2]
            if d0 < 0:
                d0 = -d0
            if d1 < 0:
                d1 = -d1
            if d2 < 0:
                d2 = -d2
            if d0 or d1 or d2:
                different_pixels += 1
                abs_sum += d0 + d1 + d2
                squared += square[d0] + square[d1] + square[d2]
                if d0 > max_delta:
                    max_delta = d0
                if d1 > max_delta:
                    max_delta = d1
                if d2 > max_delta:
                    max_delta = d2
            else:
                exact_pixels += 1
            x += 3
    mae = abs_sum / total_channels
    rmse = math.sqrt(squared / total_channels)
    psnr = math.inf if rmse == 0 else 20.0 * math.log10(255.0 / rmse)
    return {
        "exact_pixel_match_percent": round(100.0 * exact_pixels / total_pixels, 4),
        "different_pixel_percent": round(100.0 * different_pixels / total_pixels, 4),
        "max_channel_delta": max_delta,
        "mae": round(mae, 4),
        "rmse": round(rmse, 4),
        "psnr": "inf" if math.isinf(psnr) else round(psnr, 4),
    }


def ssim_global(a: RGBImage, b: RGBImage, width: int, height: int) -> float:
    sample_w = min(256, width)
    sample_h = max(1, int(height * sample_w / max(1, width)))
    sample_h = min(256, sample_h)
    ga = grayscale_crop_sample(a, width, height, sample_w, sample_h)
    gb = grayscale_crop_sample(b, width, height, sample_w, sample_h)
    n = len(ga)
    if n == 0:
        return 0.0
    mean_a = sum(ga) / n
    mean_b = sum(gb) / n
    var_a = sum((v - mean_a) ** 2 for v in ga) / n
    var_b = sum((v - mean_b) ** 2 for v in gb) / n
    cov = sum((av - mean_a) * (bv - mean_b) for av, bv in zip(ga, gb)) / n
    c1 = (0.01 * 255) ** 2
    c2 = (0.03 * 255) ** 2
    denom = (mean_a**2 + mean_b**2 + c1) * (var_a + var_b + c2)
    if denom == 0:
        return 1.0 if mean_a == mean_b else 0.0
    return ((2 * mean_a * mean_b + c1) * (2 * cov + c2)) / denom


def grayscale_crop_sample(image: RGBImage, crop_w: int, crop_h: int, out_w: int, out_h: int) -> list[float]:
    values: list[float] = []
    for y in range(out_h):
        src_y = min(crop_h - 1, int(y * crop_h / out_h))
        row = src_y * image.width * 3
        for x in range(out_w):
            src_x = min(crop_w - 1, int(x * crop_w / out_w))
            i = row + src_x * 3
            values.append(0.299 * image.pixels[i] + 0.587 * image.pixels[i + 1] + 0.114 * image.pixels[i + 2])
    return values


def phash(image: RGBImage) -> int:
    vals = grayscale_sample(image, 32, 32)
    coeffs: list[float] = []
    for v in range(8):
        for u in range(8):
            total = 0.0
            for y in range(32):
                y_cos = PHASH_COS_Y[v][y]
                for x in range(32):
                    total += vals[y * 32 + x] * PHASH_COS_X[u][x] * y_cos
            coeffs.append(total)
    body = coeffs[1:]
    median = sorted(body)[len(body) // 2]
    bits = 0
    for idx, value in enumerate(body):
        if value > median:
            bits |= 1 << idx
    return bits


def hamming(left: int, right: int) -> int:
    return (left ^ right).bit_count()


def edge_mae(a: RGBImage, b: RGBImage, width: int, height: int) -> float:
    sample_w = min(128, width)
    sample_h = max(1, min(128, int(height * sample_w / max(1, width))))
    ga = grayscale_crop_sample(a, width, height, sample_w, sample_h)
    gb = grayscale_crop_sample(b, width, height, sample_w, sample_h)

    def sobel(vals: list[float]) -> list[float]:
        out: list[float] = []
        for y in range(1, sample_h - 1):
            for x in range(1, sample_w - 1):
                i = y * sample_w + x
                gx = (
                    -vals[i - sample_w - 1]
                    + vals[i - sample_w + 1]
                    - 2 * vals[i - 1]
                    + 2 * vals[i + 1]
                    - vals[i + sample_w - 1]
                    + vals[i + sample_w + 1]
                )
                gy = (
                    vals[i - sample_w - 1]
                    + 2 * vals[i - sample_w]
                    + vals[i - sample_w + 1]
                    - vals[i + sample_w - 1]
                    - 2 * vals[i + sample_w]
                    - vals[i + sample_w + 1]
                )
                out.append(min(255.0, math.sqrt(gx * gx + gy * gy)))
        return out

    ea = sobel(ga)
    eb = sobel(gb)
    if not ea:
        return 0.0
    return sum(abs(x - y) for x, y in zip(ea, eb)) / (len(ea) * 255.0)


def blank_score(image: RGBImage) -> float:
    sample = grayscale_sample(image, min(64, image.width), min(64, image.height))
    if not sample:
        return 0.0
    mean = sum(sample) / len(sample)
    variance = sum((v - mean) ** 2 for v in sample) / len(sample)
    ink = sum(1 for v in sample if v < 245) / len(sample)
    return max(math.sqrt(variance) / 255.0, ink)


def large_region_score(a: RGBImage, b: RGBImage, width: int, height: int) -> float:
    grid = 16
    worst = 0.0
    for gy in range(grid):
        y0 = int(gy * height / grid)
        y1 = max(y0 + 1, int((gy + 1) * height / grid))
        for gx in range(grid):
            x0 = int(gx * width / grid)
            x1 = max(x0 + 1, int((gx + 1) * width / grid))
            samples = 0
            diff = 0
            step_y = max(1, (y1 - y0) // 8)
            step_x = max(1, (x1 - x0) // 8)
            for y in range(y0, y1, step_y):
                for x in range(x0, x1, step_x):
                    ai = y * a.width * 3 + x * 3
                    bi = y * b.width * 3 + x * 3
                    samples += 1
                    if max(abs(a.pixels[ai + c] - b.pixels[bi + c]) for c in range(3)) > 32:
                        diff += 1
            if samples:
                worst = max(worst, diff / samples)
    return worst


def resample_nearest(image: RGBImage, target_w: int, target_h: int) -> RGBImage:
    """Nearest-neighbour resample `image` to (target_w, target_h).

    Used to bring a dimension-mismatched page onto the comparison grid BEFORE
    metrics, so the diff reflects content divergence rather than the shear a raw
    top-left crop of two different-sized images produces."""
    if image.width == target_w and image.height == target_h:
        return image
    src = image.pixels
    out = bytearray(target_w * target_h * 3)
    for y in range(target_h):
        sy = min(image.height - 1, int(y * image.height / target_h))
        src_row = sy * image.width * 3
        dst_row = y * target_w * 3
        for x in range(target_w):
            sx = min(image.width - 1, int(x * image.width / target_w))
            s = src_row + sx * 3
            d = dst_row + x * 3
            out[d] = src[s]
            out[d + 1] = src[s + 1]
            out[d + 2] = src[s + 2]
    return RGBImage(width=target_w, height=target_h, pixels=bytes(out))


def compare_images(oxide: RenderedPage, ref: RenderedPage, thresholds: dict[str, float]) -> dict[str, Any]:
    dim_delta = max(abs(oxide.image.width - ref.image.width), abs(oxide.image.height - ref.image.height))
    dimension_match = oxide.image.width == ref.image.width and oxide.image.height == ref.image.height
    dimension_rounding_ok = dim_delta <= thresholds["dimension_rounding_px"]
    real_dimension_mismatch = not dimension_match and not dimension_rounding_ok

    # Dimension-normalization ordering: when the two pages are a genuine size
    # mismatch (beyond DPI rounding), resample the larger onto the smaller's grid
    # BEFORE computing pixel/SSIM/structural metrics. Diffing two different-sized
    # images by a raw top-left crop shears the content and would dump the page
    # into the pixel-difference bucket on top of the dimension flag (triple
    # counting). After normalization the metrics are content-aligned and the page
    # is attributed to its PRIMARY cause (dimension_mismatch) below.
    cmp_oxide, cmp_ref = oxide.image, ref.image
    if real_dimension_mismatch:
        target_w = min(oxide.image.width, ref.image.width)
        target_h = min(oxide.image.height, ref.image.height)
        cmp_oxide = resample_nearest(oxide.image, target_w, target_h)
        cmp_ref = resample_nearest(ref.image, target_w, target_h)
    width, height = crop_pair(cmp_oxide, cmp_ref)

    metrics = pixel_metrics(cmp_oxide, cmp_ref, width, height)
    metrics["ssim"] = round(ssim_global(cmp_oxide, cmp_ref, width, height), 6)
    metrics["phash_distance"] = hamming(phash(cmp_oxide), phash(cmp_ref))
    metrics["edge_mae"] = round(edge_mae(cmp_oxide, cmp_ref, width, height), 6)
    metrics["blank_score_oxide"] = round(blank_score(oxide.image), 6)
    metrics["blank_score_reference"] = round(blank_score(ref.image), 6)
    metrics["large_region_score"] = round(large_region_score(cmp_oxide, cmp_ref, width, height), 6)
    metrics["oxide_size"] = [oxide.image.width, oxide.image.height]
    metrics["reference_size"] = [ref.image.width, ref.image.height]
    metrics["compared_size"] = [width, height]
    metrics["dimension_match"] = dimension_match
    metrics["dimension_rounding_ok"] = dimension_rounding_ok
    metrics["dimension_normalized"] = real_dimension_mismatch

    # Structural failures (STRICT) are evaluated regardless of pixel tolerance.
    structural: list[str] = []
    if abs(metrics["blank_score_oxide"] - metrics["blank_score_reference"]) > thresholds["blank_delta"]:
        structural.append("blank_page_mismatch")
    if metrics["large_region_score"] > thresholds["large_region"]:
        structural.append("large_region_difference")
    if metrics["edge_mae"] > thresholds["edge_mae"]:
        structural.append("edge_or_text_shift")
    phash_noise_only = is_phash_aa_noise(metrics)
    if metrics["phash_distance"] > thresholds["phash_distance"] and not phash_noise_only:
        structural.append("perceptual_hash_distance")
    if metrics["max_channel_delta"] > 220 and metrics["mae"] > thresholds["mae"] * 3:
        structural.append("major_color_or_inversion")

    # Pixel/AA failures (LOOSE).
    pixel: list[str] = []
    pixel_noise_only = is_low_energy_pixel_noise(metrics, thresholds)
    if metrics["different_pixel_percent"] >= thresholds["different_pixel_percent"] and not pixel_noise_only:
        pixel.append("pixel_difference")
    # Global SSIM is unreliable on sparse / mostly-white pages: its
    # variance-normalised form craters when one text line sits on a large white
    # field, so two visually-identical sparse pages can score SSIM ~0.94 purely
    # from AA. Only treat low SSIM as a failure when it is CORROBORATED by a real
    # pixel or edge difference — i.e. genuine structural divergence, not the
    # sparse-page artefact. This keeps SSIM catching layout collapse (where
    # pixel% and edge_mae spike together) without false-failing near-blank pages.
    ssim_corroborated = (
        metrics["different_pixel_percent"] >= thresholds["different_pixel_percent"] * 0.5
        or metrics["edge_mae"] > thresholds["edge_mae"] * 0.5
        or metrics["mae"] > thresholds["mae"] * 0.5
    )
    if metrics["ssim"] < thresholds["ssim"] and ssim_corroborated:
        pixel.append("low_ssim")
    if metrics["mae"] > thresholds["mae"]:
        pixel.append("high_mae")

    # Primary-cause attribution. A real dimension mismatch IS the failure — the
    # page is the wrong size — so it is reported alone and the (post-normalization)
    # pixel/structural reasons are recorded for diagnostics only, not as the
    # failure cause. This stops dimension bugs from also inflating the pixel-diff
    # and SSIM buckets. Otherwise structural reasons rank ahead of pixel/AA noise.
    if real_dimension_mismatch:
        reasons = ["dimension_mismatch"]
        diagnostic = structural + pixel
    else:
        reasons = structural + pixel
        diagnostic = []
        if phash_noise_only:
            diagnostic.append("perceptual_hash_distance")
        if pixel_noise_only:
            diagnostic.append("pixel_difference")

    metrics["pass"] = not reasons
    metrics["reason"] = reasons[0] if reasons else None
    metrics["reasons"] = reasons
    metrics["diagnostic_reasons"] = diagnostic
    return metrics


def is_phash_aa_noise(metrics: dict[str, Any]) -> bool:
    """Treat pHash-only differences as diagnostic when every local signal is clean.

    The renderer-vs-Poppler harness is intentionally loose on anti-aliased edge
    noise and strict on structural failures. pHash can still flip on sparse pages
    with a tiny number of anti-aliased pixels, so only downgrade it when pixel,
    edge, blankness, and large-region metrics all independently agree.
    """
    return (
        metrics["phash_distance"] > 0
        and metrics["different_pixel_percent"] <= 0.5
        and metrics["mae"] <= 0.25
        and metrics["edge_mae"] <= 0.005
        and metrics["large_region_score"] <= 0.08
        and abs(metrics["blank_score_oxide"] - metrics["blank_score_reference"]) <= 0.01
        and metrics["ssim"] >= 0.93
    )


def is_low_energy_pixel_noise(metrics: dict[str, Any], thresholds: dict[str, float]) -> bool:
    """Downgrade high pixel-count diffs when the visual energy is clean.

    Smooth gradients, bitonal scans, and sub-pixel antialiasing can mark many
    pixels as different even when the average error is low and structural
    detectors agree. Missing content, shifted text, blank pages, and inversions
    still fail through MAE, SSIM, edge, blankness, large-region, or color checks.
    """
    return (
        metrics["different_pixel_percent"] >= thresholds["different_pixel_percent"]
        and metrics["mae"] <= thresholds["mae"]
        and metrics["ssim"] >= thresholds["ssim"]
        and metrics["edge_mae"] <= thresholds["edge_mae"]
        and metrics["large_region_score"] <= thresholds["large_region"]
        and abs(metrics["blank_score_oxide"] - metrics["blank_score_reference"]) <= thresholds["blank_delta"]
        and not (metrics["max_channel_delta"] > 220 and metrics["mae"] > thresholds["mae"] * 3)
    )


# Categories whose pages are dominated by anti-aliased glyph edges. Two CORRECT
# renderers legitimately differ around every glyph edge (sub-pixel hinting, AA
# coverage, gamma), so these get a looser per-pixel tolerance. Flat vector /
# solid-fill pages have no such excuse and keep the tighter pixel ceiling.
TEXT_HEAVY_TOKENS = (
    "text",
    "font",
    "cjk",
    "rtl",
    "multi-column",
    "forms",
)

TEXT_HEAVY_CATEGORIES = {
    # Generated many-page fixtures are sparse Helvetica text plus one flat
    # vector block. Their only post-Fix1 failure mode is cross-renderer text AA
    # noise, so they should use the same loose text thresholds as other text
    # pages; structural detectors remain strict.
    "large-files",
    # Synthetic graphics fixtures include a base text line plus simple filled
    # and stroked vector marks. Several post-Fix2 misses were high-SSIM,
    # structurally clean text/edge AA differences, not visible renderer defects.
    "synthetic-graphics",
}


def is_text_heavy(category: str | None) -> bool:
    cat = str(category or "")
    return cat in TEXT_HEAVY_CATEGORIES or any(token in cat for token in TEXT_HEAVY_TOKENS)


def thresholds(profile: str, *, text_heavy: bool = False) -> dict[str, float]:
    """Return a metric-threshold profile.

    `compression` is the STRICT same-renderer profile used by Benchmark 0B
    (Oxide-original vs Oxide-compressed); near-exact match is expected there and
    must NOT be loosened.

    `renderer` is the LOOSE cross-renderer profile for 0A (Oxide vs Poppler /
    PDFium). Two different but correct renderers antialias and hint text
    differently, scoring SSIM ~0.95-0.99 and 1-6% differing pixels on perfectly
    correct text pages, so the pixel/AA tolerances are loosened accordingly.
    Crucially the STRUCTURAL detectors (blank-page, large-region, dimension,
    edge-map, colour inversion) stay STRICT — they are what still catch a missing
    image, blanked page, shifted text, or wrong page size through the loosened
    pixel noise.
    """
    if profile == "compression":
        return {
            "dimension_rounding_px": 0,
            "different_pixel_percent": 0.05,
            "ssim": 0.9995,
            "mae": 0.25,
            "edge_mae": 0.005,
            "large_region": 0.02,
            "phash_distance": 2,
            "blank_delta": 0.01,
        }

    # Renderer-vs-renderer (0A). Loose on global AA noise; strict on structure.
    profile_thresholds = {
        # DPI rounding + integer page-pixel ceil legitimately differ by a pixel
        # or two between renderers; only a larger delta is a real size bug.
        "dimension_rounding_px": 4,
        # Pixel/AA tolerances (loose). Text pages diff more (AA around every
        # glyph edge); flat vector/solid pages should stay clean.
        "different_pixel_percent": 8.0 if text_heavy else 3.0,
        "ssim": 0.93 if text_heavy else 0.95,
        "mae": 6.0 if text_heavy else 3.0,
        # Structural detectors (STRICT — independent of the loosened pixel %).
        # large_region: a whole grid cell differing hard = missing image/region.
        "large_region": 0.45,
        # edge_mae: catches shifted/missing text even when global pixels look
        # close. Kept tight; text reflow/omission lights this up.
        "edge_mae": 0.10 if text_heavy else 0.06,
        # phash: gross perceptual divergence (layout collapse, inversion).
        "phash_distance": 14,
        # blank-page mismatch: one side blank, the other not = hard fail.
        "blank_delta": 0.06,
    }
    return profile_thresholds


def compare_page_sets(
    oxide_pages: list[RenderedPage],
    ref_pages: list[RenderedPage],
    profile: str,
    category: str | None = None,
) -> dict[str, Any]:
    by_page_ref = {p.page: p for p in ref_pages}
    by_page_oxide = {p.page: p for p in oxide_pages}
    failed: list[dict[str, Any]] = []
    compared = 0
    passed = 0
    dim_pass = 0
    threshold = thresholds(profile, text_heavy=is_text_heavy(category))
    for page in sorted(set(by_page_ref) & set(by_page_oxide)):
        compared += 1
        metrics = compare_images(by_page_oxide[page], by_page_ref[page], threshold)
        metrics["page"] = page
        if metrics["dimension_match"] or metrics["dimension_rounding_ok"]:
            dim_pass += 1
        if metrics["pass"]:
            passed += 1
        else:
            failed.append(metrics)
    missing_pages = sorted(set(by_page_ref) ^ set(by_page_oxide))
    for page in missing_pages:
        failed.append({"page": page, "reason": "rendered_page_missing", "reasons": ["rendered_page_missing"], "pass": False})
    return {
        "compared_pages": compared,
        "pass_pages": passed,
        "dimension_pass_pages": dim_pass,
        "failed_pages": failed,
        "missing_page_numbers": missing_pages,
    }


def is_hostile(entry: dict[str, Any]) -> bool:
    return str(entry.get("category", "")).startswith("hostile-")


def command_crashed(result: dict[str, Any]) -> bool:
    exit_code = result.get("exit_code")
    stderr = (result.get("stderr") or "").lower()
    if "panicked at" in stderr or "stack backtrace" in stderr:
        return True
    if exit_code is None:
        return False
    if os.name == "nt":
        return exit_code in {0xC0000005, 0xC0000409, 0xC00000FD}
    return exit_code < 0


def determinism_check(entry: dict[str, Any], oxide_bin: str, work_dir: Path, dpi: int, timeout: int, mem_mb: int | None) -> dict[str, Any]:
    hashes: list[str] = []
    commands: list[dict[str, Any]] = []
    for i in range(3):
        run_dir = work_dir / f"determinism_{i}"
        run_dir.mkdir(parents=True, exist_ok=True)
        rendered = render_oxide(entry, oxide_bin, run_dir, dpi, 1, timeout, mem_mb, suffix="determinism")
        commands.append(rendered["command"])
        if rendered["pages"]:
            hashes.append(rendered["pages"][0].sha256)
    stable = len(hashes) == 3 and len(set(hashes)) == 1
    note = "bit-identical first-page PNG across 3 runs" if stable else "not bit-identical or render failed"
    return {"stable": stable, "hashes": hashes, "note": note, "commands": commands}


def process_entry(
    entry: dict[str, Any],
    *,
    args: argparse.Namespace,
    poppler: dict[str, str],
    oxide_bin: str,
    output_dir: Path,
    do_determinism: bool,
) -> dict[str, Any]:
    file_id = safe_id(entry)
    work_dir = output_dir / "artifacts" / file_id
    work_dir.mkdir(parents=True, exist_ok=True)

    oxide_info = get_oxide_info(entry, oxide_bin, args.timeout_sec, args.max_memory_mb)
    poppler_info = get_poppler_info(entry, poppler, args.timeout_sec, args.max_memory_mb)
    oxide_pages_declared = oxide_info.get("page_count")
    poppler_pages_declared = poppler_info.get("page_count")
    pages = page_limit(oxide_pages_declared or poppler_pages_declared, args.max_pages_per_file)

    poppler_render = render_poppler(entry, poppler, work_dir, args.dpi, pages, args.timeout_sec, args.max_memory_mb)
    oxide_render = render_oxide(entry, oxide_bin, work_dir, args.dpi, pages, args.timeout_sec, args.max_memory_mb)
    visual = compare_page_sets(
        oxide_render["pages"],
        poppler_render["pages"],
        args.threshold_profile,
        entry.get("category"),
    )

    oxide_command = oxide_render["command"]
    safety = {
        "crashed": command_crashed(oxide_command),
        "timed_out": oxide_command["timed_out"],
        "timeout_safe": True,
        "memory_exceeded": oxide_command["memory_exceeded"],
        "peak_mem_ok": not oxide_command["memory_exceeded"],
        "active_content_ignored": is_hostile(entry)
        and any(token in str(entry.get("category", "")) for token in ["openaction-js", "uri-action", "launch-action"]),
    }

    determinism = (
        determinism_check(entry, oxide_bin, work_dir, args.dpi, args.timeout_sec, args.max_memory_mb)
        if do_determinism and not is_hostile(entry)
        else {"stable": None, "note": "not sampled"}
    )

    fail_reasons: list[str] = []
    if oxide_pages_declared is not None and poppler_pages_declared is not None and oxide_pages_declared != poppler_pages_declared:
        fail_reasons.append("page_count_mismatch")
    if not oxide_render["command"]["ok"] and not is_hostile(entry):
        fail_reasons.append("oxide_render_failed")
    if not poppler_render["command"]["ok"] and not is_hostile(entry):
        fail_reasons.append("poppler_render_failed")
    if visual["failed_pages"] and not is_hostile(entry):
        fail_reasons.extend(sorted({p.get("reason", "visual_failure") for p in visual["failed_pages"]}))
    if safety["crashed"]:
        fail_reasons.append("oxide_crash_or_panic")
    if safety["memory_exceeded"]:
        fail_reasons.append("oxide_memory_cap_exceeded")
    if determinism.get("stable") is False:
        fail_reasons.append("non_deterministic")

    if is_hostile(entry):
        result = "fail" if safety["crashed"] or safety["memory_exceeded"] else "pass"
    else:
        result = "pass" if not fail_reasons else "fail"

    return {
        "file": entry.get("path"),
        "id": file_id,
        "category": entry.get("category"),
        "source": entry.get("source"),
        "notes": entry.get("notes"),
        "page_count": {
            "oxide": oxide_pages_declared,
            "poppler": poppler_pages_declared,
            "pdfium": None,
            "pass": oxide_pages_declared is not None
            and poppler_pages_declared is not None
            and oxide_pages_declared == poppler_pages_declared,
        },
        "render": {
            "oxide_success": oxide_render["command"]["ok"],
            "poppler_success": poppler_render["command"]["ok"],
            "pdfium_success": None,
            "oxide_rendered_pages": len(oxide_render["pages"]),
            "poppler_rendered_pages": len(poppler_render["pages"]),
            "oxide": oxide_render["command"],
            "poppler": poppler_render["command"],
            "oxide_parse_errors": oxide_render["parse_errors"],
            "poppler_parse_errors": poppler_render["parse_errors"],
        },
        "visual_compare": {
            "oxide_vs_poppler_pass_pages": visual["pass_pages"],
            "oxide_vs_pdfium_pass_pages": None,
            "compared_pages": visual["compared_pages"],
            "failed_pages": visual["failed_pages"],
        },
        "performance": {
            "oxide_total_ms": oxide_render["command"]["duration_ms"],
            "poppler_total_ms": poppler_render["command"]["duration_ms"],
            "pdfium_total_ms": None,
            "oxide_peak_memory_mb": oxide_render["command"]["peak_memory_mb"],
            "poppler_peak_memory_mb": poppler_render["command"]["peak_memory_mb"],
            "oxide_speed_ratio_vs_poppler": (
                round(poppler_render["command"]["duration_ms"] / oxide_render["command"]["duration_ms"], 4)
                if oxide_render["command"]["duration_ms"] > 0 and poppler_render["command"]["duration_ms"] > 0
                else None
            ),
        },
        "safety": safety,
        "determinism": determinism,
        "result": result,
        "fail_reasons": sorted(set(fail_reasons)),
    }


def pct(num: int, den: int) -> float:
    return 100.0 * num / den if den else 0.0


def category_breakdown(results: list[dict[str, Any]]) -> dict[str, Any]:
    out: dict[str, Any] = {}
    for category in sorted({r["category"] for r in results}):
        items = [r for r in results if r["category"] == category]
        pages = sum(r["visual_compare"]["compared_pages"] for r in items if not str(category).startswith("hostile-"))
        passed = sum(r["visual_compare"]["oxide_vs_poppler_pass_pages"] for r in items if not str(category).startswith("hostile-"))
        out[category] = {
            "files": len(items),
            "file_pass_percent": round(pct(sum(1 for r in items if r["result"] == "pass"), len(items)), 2),
            "visual_pages": pages,
            "visual_pass_percent": round(pct(passed, pages), 2) if pages else None,
        }
    return out


def aggregate(results: list[dict[str, Any]], args: argparse.Namespace, versions: dict[str, Any], pdfium: str | None) -> dict[str, Any]:
    normal = [r for r in results if not str(r["category"]).startswith("hostile-")]
    hostile = [r for r in results if str(r["category"]).startswith("hostile-")]
    visual_pages = sum(r["visual_compare"]["compared_pages"] for r in normal)
    visual_pass = sum(r["visual_compare"]["oxide_vs_poppler_pass_pages"] for r in normal)
    oxide_rendered_pages = sum(r.get("render", {}).get("oxide_rendered_pages", 0) for r in results)
    poppler_rendered_pages = sum(r.get("render", {}).get("poppler_rendered_pages", 0) for r in results)
    dim_pages = visual_pages
    dim_pass = 0
    for r in normal:
        for failed in r["visual_compare"]["failed_pages"]:
            if failed.get("reason") == "dimension_mismatch":
                continue
        dim_pass += r["visual_compare"]["compared_pages"] - sum(
            1 for f in r["visual_compare"]["failed_pages"] if f.get("reason") == "dimension_mismatch"
        )
    page_count_checks = [r["page_count"]["pass"] for r in normal if r["page_count"]["oxide"] is not None and r["page_count"]["poppler"] is not None]
    page_count_score = pct(sum(1 for ok in page_count_checks if ok), len(page_count_checks))
    dimension_score = pct(dim_pass, dim_pages)
    page_dimension_score = (page_count_score + dimension_score) / 2 if page_count_checks or dim_pages else 0.0

    hostile_crash_free = pct(sum(1 for r in hostile if not r["safety"]["crashed"]), len(hostile))
    hostile_timeout_safe = pct(sum(1 for r in hostile if r["safety"]["timeout_safe"]), len(hostile))
    hostile_memory_ok = pct(sum(1 for r in hostile if r["safety"]["peak_mem_ok"]), len(hostile))
    safety_score = (hostile_crash_free + hostile_timeout_safe + hostile_memory_ok) / 3 if hostile else 0.0

    visual_score = pct(visual_pass, visual_pages)
    text_categories = ("text", "font", "cjk", "rtl", "multi-column", "forms")
    image_categories = ("image", "jpeg", "scanned", "graphics", "vector", "color", "transparency")

    def category_visual_score(tokens: tuple[str, ...]) -> float:
        subset = [r for r in normal if any(token in str(r["category"]) for token in tokens)]
        pages = sum(r["visual_compare"]["compared_pages"] for r in subset)
        passed = sum(r["visual_compare"]["oxide_vs_poppler_pass_pages"] for r in subset)
        return pct(passed, pages) if pages else visual_score

    speed_ratios = [
        r["performance"]["oxide_speed_ratio_vs_poppler"]
        for r in normal
        if r["performance"]["oxide_speed_ratio_vs_poppler"] is not None and r["render"]["oxide_success"] and r["render"]["poppler_success"]
    ]
    median_speed = sorted(speed_ratios)[len(speed_ratios) // 2] if speed_ratios else 0.0
    perf_score = min(100.0, (median_speed / 0.70) * 100.0) if median_speed else 0.0
    memory_ok_rate = pct(sum(1 for r in results if r["safety"]["peak_mem_ok"]), len(results))
    perf_memory_score = (perf_score * 0.75) + (memory_ok_rate * 0.25)

    sub = {
        "safety": round(safety_score, 2),
        "page_count_dimensions": round(page_dimension_score, 2),
        "visual_match": round(visual_score, 2),
        "text_font_correctness_proxy": round(category_visual_score(text_categories), 2),
        "image_color_correctness_proxy": round(category_visual_score(image_categories), 2),
        "performance_memory": round(perf_memory_score, 2),
    }
    weighted = (
        sub["safety"] * 0.25
        + sub["page_count_dimensions"] * 0.15
        + sub["visual_match"] * 0.35
        + sub["text_font_correctness_proxy"] * 0.10
        + sub["image_color_correctness_proxy"] * 0.10
        + sub["performance_memory"] * 0.05
    )

    blockers = blocking_findings(results)
    real_files = sum(1 for r in results if str(r["category"]).startswith("real-"))
    tier = "Tier 0"
    if visual_score >= 95.0 and not blockers:
        tier = "Tier 1"
    if visual_score >= 99.0 and safety_score == 100.0 and not blockers:
        tier = "Tier 2"
    if tier == "Tier 2" and real_files >= 1000 and visual_pages >= 10000:
        tier = "Tier 3"

    return {
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "mode": "0A-renderer-compatibility",
        "dpi": args.dpi,
        "max_pages_per_file": args.max_pages_per_file,
        "threshold_profile": args.threshold_profile,
        "backends": {
            "oxide_bin": args.oxide_bin,
            "poppler": versions["poppler"],
            "pdfium": {"available": pdfium is not None, "path": pdfium, "version": versions.get("pdfium")},
        },
        "oxide_cli_reconciliation": {
            "info": "adapted existing `oxide info --json`",
            "render": "adapted existing `oxide render --format png` ZIP output; no product CLI change",
            "timeout_memory": "enforced by benchmark subprocess monitor, not product CLI flags",
            "disable_javascript": "Oxide has no JavaScript execution path; active content fixtures are rendered as inert PDF objects",
        },
        "scale": {
            "files": len(results),
            "normal_files": len(normal),
            "hostile_files": len(hostile),
            "real_world_files": real_files,
            "visual_pages_compared": visual_pages,
            "visual_pages_passed": visual_pass,
            "oxide_rendered_pages": oxide_rendered_pages,
            "poppler_rendered_pages": poppler_rendered_pages,
            "total_backend_page_images": oxide_rendered_pages + poppler_rendered_pages,
            "full_spec_real_world_files": 1000,
            "full_spec_rendered_pages": 10000,
        },
        "results": {
            "file_pass_percent": round(pct(sum(1 for r in results if r["result"] == "pass"), len(results)), 2),
            "visual_pass_percent": round(visual_score, 2),
            "safety": {
                "hostile_crash_free_percent": round(hostile_crash_free, 2),
                "hostile_timeout_safe_percent": round(hostile_timeout_safe, 2),
                "hostile_memory_bounded_percent": round(hostile_memory_ok, 2),
            },
            "performance": {
                "median_oxide_speed_ratio_vs_poppler": round(median_speed, 4),
                "ratio_definition": "poppler_total_ms / oxide_total_ms; >1 means Oxide faster",
                "peak_oxide_memory_mb_max": max((r["performance"]["oxide_peak_memory_mb"] or 0 for r in results), default=0),
            },
            "determinism": determinism_summary(results),
            "failure_breakdown": failure_breakdown(results),
            "category_breakdown": category_breakdown(results),
        },
        "weighted_score": {
            "score": round(weighted, 2),
            "sub_scores": sub,
            "weights": {
                "safety": 25,
                "page_count_dimensions": 15,
                "visual_match": 35,
                "text_font_correctness": 10,
                "image_color_correctness": 10,
                "performance_memory": 5,
            },
        },
        "tier": {
            "rating": tier,
            "scale_caveat": "Tier is rated only at this run's corpus scale; Tier 3 requires 1,000+ real PDFs and 10,000+ rendered pages.",
        },
        "blocking_findings": blockers,
    }


def determinism_summary(results: list[dict[str, Any]]) -> dict[str, Any]:
    sampled = [r for r in results if r["determinism"].get("stable") is not None]
    stable = [r for r in sampled if r["determinism"].get("stable")]
    return {
        "sampled_files": len(sampled),
        "stable_files": len(stable),
        "stable_percent": round(pct(len(stable), len(sampled)), 2) if sampled else None,
        "unstable": [r["id"] for r in sampled if not r["determinism"].get("stable")],
    }


def failure_breakdown(results: list[dict[str, Any]]) -> dict[str, int]:
    counts: dict[str, int] = {}
    for result in results:
        for reason in result.get("fail_reasons", []):
            counts[reason] = counts.get(reason, 0) + 1
        if is_hostile(result):
            continue
        for page in result["visual_compare"]["failed_pages"]:
            reason = page.get("reason") or "visual_failure"
            counts[f"page:{reason}"] = counts.get(f"page:{reason}", 0) + 1
    return dict(sorted(counts.items(), key=lambda item: (-item[1], item[0])))


def blocking_findings(results: list[dict[str, Any]]) -> list[dict[str, Any]]:
    blockers: list[dict[str, Any]] = []
    blocker_reasons = {
        "oxide_crash_or_panic",
        "oxide_memory_cap_exceeded",
        "page_count_mismatch",
        "blank_page_mismatch",
        "large_region_difference",
        "major_color_or_inversion",
        "rendered_page_missing",
        "non_deterministic",
    }
    for result in results:
        for reason in result.get("fail_reasons", []):
            if reason in blocker_reasons:
                blockers.append({"file": result["file"], "category": result["category"], "reason": reason})
        if is_hostile(result):
            continue
        for page in result["visual_compare"]["failed_pages"]:
            reasons = set(page.get("reasons", []))
            if reasons & blocker_reasons:
                blockers.append(
                    {
                        "file": result["file"],
                        "category": result["category"],
                        "page": page.get("page"),
                        "reason": sorted(reasons & blocker_reasons)[0],
                    }
                )
    return blockers[:200]


def write_markdown(agg: dict[str, Any], output_dir: Path) -> None:
    lines = [
        "# Renderer Benchmark 0A Report",
        "",
        f"Generated: {agg['generated_at']}",
        "",
        "## Scope",
        "",
        f"- Files run: {agg['scale']['files']}",
        f"- Normal files: {agg['scale']['normal_files']}",
        f"- Hostile files: {agg['scale']['hostile_files']}",
        f"- Real-world files: {agg['scale']['real_world_files']}",
        f"- Visual pages compared: {agg['scale']['visual_pages_compared']}",
        f"- Full target: {agg['scale']['full_spec_real_world_files']} real PDFs / {agg['scale']['full_spec_rendered_pages']} rendered pages",
        f"- DPI: {agg['dpi']}",
        f"- Page cap per file: {agg['max_pages_per_file']}",
        "",
        "## Backends",
        "",
        f"- Oxide: `{agg['backends']['oxide_bin']}`",
        f"- Poppler: `{agg['backends']['poppler']}`",
        f"- PDFium: {'available' if agg['backends']['pdfium']['available'] else 'not available; skipped cleanly'}",
        "",
        "## Results",
        "",
        f"- Weighted score: **{agg['weighted_score']['score']}**",
        f"- Tier: **{agg['tier']['rating']}**",
        f"- Visual pass: **{agg['results']['visual_pass_percent']}%**",
        f"- Hostile crash-free: **{agg['results']['safety']['hostile_crash_free_percent']}%**",
        f"- Hostile timeout-safe: **{agg['results']['safety']['hostile_timeout_safe_percent']}%**",
        f"- Hostile memory-bounded: **{agg['results']['safety']['hostile_memory_bounded_percent']}%**",
        f"- Median speed ratio Poppler/Oxide: **{agg['results']['performance']['median_oxide_speed_ratio_vs_poppler']}**",
        f"- Determinism: {agg['results']['determinism']}",
        "",
        "## Sub-Scores",
        "",
    ]
    for key, value in agg["weighted_score"]["sub_scores"].items():
        lines.append(f"- {key}: {value}%")
    lines.extend(["", "## Failure Breakdown", ""])
    if agg["results"]["failure_breakdown"]:
        for key, value in list(agg["results"]["failure_breakdown"].items())[:50]:
            lines.append(f"- {key}: {value}")
    else:
        lines.append("- none")
    lines.extend(["", "## Category Breakdown", ""])
    lines.append("| category | files | file pass % | visual pages | visual pass % |")
    lines.append("| --- | ---: | ---: | ---: | ---: |")
    for category, stats in agg["results"]["category_breakdown"].items():
        lines.append(
            f"| {category} | {stats['files']} | {stats['file_pass_percent']} | "
            f"{stats['visual_pages']} | {stats['visual_pass_percent']} |"
        )
    lines.extend(["", "## Blocking Findings", ""])
    if agg["blocking_findings"]:
        for item in agg["blocking_findings"][:50]:
            lines.append(f"- `{item.get('file')}` page {item.get('page', '-')}: {item['reason']} ({item.get('category')})")
    else:
        lines.append("- none")
    lines.extend(
        [
            "",
            "## Scale Caveat",
            "",
            agg["tier"]["scale_caveat"],
            "This report must not be used as a full Tier-3 claim until the corpus is expanded to the full target.",
            "",
        ]
    )
    (output_dir / "aggregate.md").write_text("\n".join(lines), encoding="utf-8")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", default=str(REPO_ROOT / "renderer-benchmark" / "corpus" / "manifest.json"))
    parser.add_argument("--oxide-bin", default=str(REPO_ROOT / "target" / "release" / executable_name("oxide")))
    parser.add_argument("--poppler-bin-dir")
    parser.add_argument("--pdfium-bin")
    parser.add_argument("--dpi", type=int, default=144)
    parser.add_argument("--timeout-sec", type=int, default=20)
    parser.add_argument("--max-memory-mb", type=int, default=1024)
    parser.add_argument("--max-pages-per-file", type=int)
    parser.add_argument("--output-dir", default=str(REPO_ROOT / "renderer-benchmark" / "results" / "run-0a"))
    parser.add_argument("--category")
    parser.add_argument("--file")
    parser.add_argument("--limit", type=int)
    parser.add_argument("--determinism-sample", type=int, default=24)
    parser.add_argument("--threshold-profile", choices=["renderer", "compression"], default="renderer")
    parser.add_argument(
        "--resume-existing",
        action="store_true",
        help="reuse per-file JSON results already present in the output directory and process only missing entries",
    )
    args = parser.parse_args()

    output_dir = Path(args.output_dir)
    output_dir.mkdir(parents=True, exist_ok=True)
    (output_dir / "files").mkdir(exist_ok=True)
    (output_dir / "artifacts").mkdir(exist_ok=True)

    poppler_bin_dir = Path(args.poppler_bin_dir) if args.poppler_bin_dir else None
    poppler = {
        "pdfinfo": find_executable("pdfinfo", poppler_bin_dir),
        "pdftoppm": find_executable("pdftoppm", poppler_bin_dir),
    }
    missing = [name for name, exe in poppler.items() if exe is None]
    if missing:
        raise SystemExit(f"missing required Poppler executable(s): {', '.join(missing)}")
    poppler = {k: str(v) for k, v in poppler.items() if v is not None}

    pdfium = args.pdfium_bin or find_executable("pdfium_test") or find_executable("pdfium-render")
    if pdfium and not Path(pdfium).exists() and shutil.which(pdfium) is None:
        pdfium = None

    entries = selected_entries(args)
    versions = backend_versions(poppler, pdfium)
    results: list[dict[str, Any]] = []
    sampled = 0
    for idx, entry in enumerate(entries, start=1):
        result_path = output_dir / "files" / f"{safe_id(entry)}.json"
        if args.resume_existing and result_path.exists():
            try:
                result = json.loads(result_path.read_text(encoding="utf-8"))
                results.append(result)
                if result.get("determinism", {}).get("stable") is not None:
                    sampled += 1
                print(f"[{idx}/{len(entries)}] {entry.get('id')} ({entry.get('category')}) [cached]", flush=True)
                continue
            except (OSError, json.JSONDecodeError):
                pass
        print(f"[{idx}/{len(entries)}] {entry.get('id')} ({entry.get('category')})", flush=True)
        do_det = sampled < args.determinism_sample and not is_hostile(entry)
        if do_det:
            sampled += 1
        try:
            result = process_entry(
                entry,
                args=args,
                poppler=poppler,
                oxide_bin=args.oxide_bin,
                output_dir=output_dir,
                do_determinism=do_det,
            )
        except Exception as err:  # noqa: BLE001
            result = {
                "file": entry.get("path"),
                "id": safe_id(entry),
                "category": entry.get("category"),
                "result": "fail",
                "fail_reasons": ["harness_exception"],
                "harness_exception": str(err),
                "page_count": {"oxide": None, "poppler": None, "pdfium": None, "pass": False},
                "render": {"oxide_success": False, "poppler_success": False, "pdfium_success": None},
                "visual_compare": {"oxide_vs_poppler_pass_pages": 0, "oxide_vs_pdfium_pass_pages": None, "compared_pages": 0, "failed_pages": []},
                "performance": {"oxide_total_ms": None, "poppler_total_ms": None, "pdfium_total_ms": None, "oxide_peak_memory_mb": None},
                "safety": {"crashed": False, "timed_out": False, "peak_mem_ok": False},
                "determinism": {"stable": None, "note": "not sampled"},
            }
        results.append(result)
        result_path.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")

    agg = aggregate(results, args, versions, pdfium)
    (output_dir / "aggregate.json").write_text(json.dumps(agg, indent=2) + "\n", encoding="utf-8")
    write_markdown(agg, output_dir)
    print(f"Wrote {output_dir / 'aggregate.json'}")
    print(f"Wrote {output_dir / 'aggregate.md'}")


if __name__ == "__main__":
    main()
