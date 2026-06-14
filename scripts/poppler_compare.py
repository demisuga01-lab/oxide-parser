#!/usr/bin/env python3
"""Compare Oxide CLI output against Poppler for a tagged PDF corpus."""

from __future__ import annotations

import argparse
import csv
import difflib
import json
import math
import os
import re
import shutil
import struct
import subprocess
import sys
import time
import zipfile
import zlib
from collections import Counter
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


REPO_ROOT = Path(__file__).resolve().parents[1]
PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"


@dataclass
class CommandResult:
    ok: bool
    exit_code: int | None
    timed_out: bool
    duration_seconds: float
    stdout: str
    stderr: str
    error: str | None = None

    def compact(self) -> dict[str, Any]:
        return {
            "ok": self.ok,
            "exit_code": self.exit_code,
            "timed_out": self.timed_out,
            "duration_seconds": round(self.duration_seconds, 3),
            "stdout": trim_text(self.stdout),
            "stderr": trim_text(self.stderr),
            "error": self.error,
        }


@dataclass
class RGBImage:
    width: int
    height: int
    pixels: bytes


def trim_text(value: str, limit: int = 4000) -> str:
    if len(value) <= limit:
        return value
    return value[:limit] + f"\n... truncated {len(value) - limit} chars ..."


def run_command(args: list[str], timeout: int, cwd: Path = REPO_ROOT) -> CommandResult:
    start = time.monotonic()
    try:
        proc = subprocess.run(
            args,
            cwd=str(cwd),
            timeout=timeout,
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
        )
        duration = time.monotonic() - start
        return CommandResult(
            ok=proc.returncode == 0,
            exit_code=proc.returncode,
            timed_out=False,
            duration_seconds=duration,
            stdout=proc.stdout,
            stderr=proc.stderr,
        )
    except subprocess.TimeoutExpired as err:
        duration = time.monotonic() - start
        return CommandResult(
            ok=False,
            exit_code=None,
            timed_out=True,
            duration_seconds=duration,
            stdout=(err.stdout or "") if isinstance(err.stdout, str) else "",
            stderr=(err.stderr or "") if isinstance(err.stderr, str) else "",
            error=f"timeout after {timeout}s",
        )
    except FileNotFoundError as err:
        duration = time.monotonic() - start
        return CommandResult(
            ok=False,
            exit_code=None,
            timed_out=False,
            duration_seconds=duration,
            stdout="",
            stderr="",
            error=str(err),
        )


def executable_name(name: str) -> str:
    if os.name == "nt" and not name.lower().endswith(".exe"):
        return name + ".exe"
    return name


def find_executable(name: str, bin_dir: Path | None) -> str | None:
    exe_name = executable_name(name)
    if bin_dir:
        candidate = bin_dir / exe_name
        if candidate.exists():
            return str(candidate)
    found = shutil.which(name)
    if found:
        return found
    return None


def resolve_oxide_bin(args: argparse.Namespace) -> str:
    if args.oxide_bin:
        oxide = Path(args.oxide_bin)
        if not oxide.exists():
            raise SystemExit(f"oxide binary not found: {oxide}")
        return str(oxide)

    target_name = executable_name("oxide")
    debug_bin = REPO_ROOT / "target" / "debug" / target_name
    release_bin = REPO_ROOT / "target" / "release" / target_name
    if release_bin.exists():
        return str(release_bin)
    if debug_bin.exists():
        return str(debug_bin)

    if args.no_build:
        raise SystemExit(
            "oxide binary was not found. Build it first or omit --no-build.\n"
            "Expected target/debug/oxide(.exe) or target/release/oxide(.exe)."
        )

    print("Building oxide CLI with `cargo build -p oxide-cli`...", file=sys.stderr)
    build = run_command(["cargo", "build", "-p", "oxide-cli"], timeout=args.build_timeout)
    if not build.ok:
        raise SystemExit(
            "failed to build oxide CLI\n"
            + trim_text(build.stdout)
            + "\n"
            + trim_text(build.stderr)
        )
    if not debug_bin.exists():
        raise SystemExit(f"cargo build completed but {debug_bin} does not exist")
    return str(debug_bin)


def load_entries(args: argparse.Namespace) -> tuple[dict[str, Any], list[dict[str, Any]]]:
    manifest: dict[str, Any]
    entries: list[dict[str, Any]]
    if args.input_dir:
        input_dir = Path(args.input_dir or ".")
        if not input_dir.is_absolute():
            input_dir = REPO_ROOT / input_dir
        entries = [
            {
                "id": pdf.stem,
                "path": str(pdf.relative_to(REPO_ROOT)),
                "absolute_path": str(pdf.resolve()),
                "category": "uncategorized",
                "source": "input-dir",
            }
            for pdf in sorted(input_dir.rglob("*.pdf"))
        ]
        manifest = {
            "version": 1,
            "description": "Ad hoc input directory without category metadata.",
        }
    elif args.manifest:
        manifest_path = Path(args.manifest)
        if not manifest_path.is_absolute():
            manifest_path = REPO_ROOT / manifest_path
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        base = manifest_path.parent
        entries = []
        for raw in manifest.get("entries", []):
            entry = dict(raw)
            path = Path(entry["path"])
            if not path.is_absolute():
                repo_path = REPO_ROOT / path
                local_path = base / path
                path = repo_path if repo_path.exists() else local_path
            entry["absolute_path"] = str(path.resolve())
            entries.append(entry)
    else:
        raise SystemExit("provide --manifest or --input-dir")

    categories = split_filter(args.category)
    file_filters = split_filter(args.file)
    if categories:
        entries = [e for e in entries if e.get("category") in categories]
    if file_filters:
        lowered = [f.lower() for f in file_filters]

        def matches(entry: dict[str, Any]) -> bool:
            haystacks = [
                str(entry.get("id", "")).lower(),
                Path(entry.get("path", "")).name.lower(),
                str(entry.get("path", "")).lower(),
            ]
            return any(f in h for f in lowered for h in haystacks)

        entries = [e for e in entries if matches(e)]
    if args.limit is not None:
        entries = entries[: args.limit]
    return manifest, entries


def split_filter(value: str | None) -> list[str]:
    if not value:
        return []
    return [part.strip() for part in value.split(",") if part.strip()]


def normalize_text(text: str) -> str:
    text = text.replace("\r\n", "\n").replace("\r", "\n")
    text = re.sub(r"[ \t\f\v]+", " ", text)
    text = re.sub(r"\n{3,}", "\n\n", text)
    return text.strip()


def tokenize_text(text: str) -> list[str]:
    return re.findall(r"\w+|[^\w\s]", normalize_text(text).lower(), flags=re.UNICODE)


def text_similarity(poppler_text: str, oxide_text: str) -> dict[str, float]:
    poppler_norm = normalize_text(poppler_text)
    oxide_norm = normalize_text(oxide_text)
    if not poppler_norm and not oxide_norm:
        return {"word_ratio": 1.0, "char_ratio": 1.0}
    poppler_tokens = tokenize_text(poppler_norm)
    oxide_tokens = tokenize_text(oxide_norm)
    if max(len(poppler_tokens), len(oxide_tokens)) > 5000:
        word_ratio = token_dice_similarity(poppler_tokens, oxide_tokens)
    else:
        word_ratio = difflib.SequenceMatcher(None, poppler_tokens, oxide_tokens).ratio()
    if max(len(poppler_norm), len(oxide_norm)) > 20000:
        char_ratio = word_ratio
    else:
        char_ratio = difflib.SequenceMatcher(None, poppler_norm, oxide_norm).ratio()
    return {"word_ratio": word_ratio, "char_ratio": char_ratio}


def token_dice_similarity(left: list[str], right: list[str]) -> float:
    if not left and not right:
        return 1.0
    if not left or not right:
        return 0.0
    left_counts = Counter(left)
    right_counts = Counter(right)
    overlap = sum(min(count, right_counts[token]) for token, count in left_counts.items())
    return (2.0 * overlap) / (len(left) + len(right))


def read_text(path: Path) -> str:
    if not path.exists():
        return ""
    return path.read_text(encoding="utf-8", errors="replace")


def parse_ppm(path: Path) -> RGBImage:
    data = path.read_bytes()
    pos = 0

    def next_token() -> bytes:
        nonlocal pos
        while pos < len(data):
            byte = data[pos]
            if byte == ord("#"):
                while pos < len(data) and data[pos] not in (10, 13):
                    pos += 1
            elif chr(byte).isspace():
                pos += 1
            else:
                break
        start = pos
        while pos < len(data) and not chr(data[pos]).isspace():
            pos += 1
        return data[start:pos]

    magic = next_token()
    if magic != b"P6":
        raise ValueError(f"{path} is not a binary PPM (P6)")
    width = int(next_token())
    height = int(next_token())
    maxval = int(next_token())
    if maxval != 255:
        raise ValueError(f"{path} has unsupported PPM maxval {maxval}")
    while pos < len(data) and chr(data[pos]).isspace():
        pos += 1
    pixel_count = width * height * 3
    pixels = data[pos : pos + pixel_count]
    if len(pixels) != pixel_count:
        raise ValueError(f"{path} has truncated PPM pixel data")
    return RGBImage(width, height, pixels)


def paeth(left: int, above: int, upper_left: int) -> int:
    p = left + above - upper_left
    pa = abs(p - left)
    pb = abs(p - above)
    pc = abs(p - upper_left)
    if pa <= pb and pa <= pc:
        return left
    if pb <= pc:
        return above
    return upper_left


def parse_png_bytes(data: bytes) -> RGBImage:
    if not data.startswith(PNG_SIGNATURE):
        raise ValueError("not a PNG file")
    pos = len(PNG_SIGNATURE)
    width = height = bit_depth = color_type = None
    idat = bytearray()
    while pos < len(data):
        if pos + 8 > len(data):
            raise ValueError("truncated PNG chunk header")
        length = struct.unpack(">I", data[pos : pos + 4])[0]
        chunk_type = data[pos + 4 : pos + 8]
        pos += 8
        chunk = data[pos : pos + length]
        pos += length + 4
        if chunk_type == b"IHDR":
            width, height, bit_depth, color_type, compression, png_filter, interlace = struct.unpack(
                ">IIBBBBB", chunk
            )
            if compression != 0 or png_filter != 0 or interlace != 0:
                raise ValueError("unsupported PNG compression, filter, or interlace mode")
        elif chunk_type == b"IDAT":
            idat.extend(chunk)
        elif chunk_type == b"IEND":
            break

    if width is None or height is None or bit_depth is None or color_type is None:
        raise ValueError("PNG missing IHDR")
    if bit_depth != 8:
        raise ValueError(f"unsupported PNG bit depth {bit_depth}")
    channels_by_type = {0: 1, 2: 3, 4: 2, 6: 4}
    if color_type not in channels_by_type:
        raise ValueError(f"unsupported PNG color type {color_type}")
    channels = channels_by_type[color_type]
    bpp = channels
    scanline_len = width * channels
    raw = zlib.decompress(bytes(idat))
    expected = (scanline_len + 1) * height
    if len(raw) < expected:
        raise ValueError("truncated PNG image data")

    rows: list[bytearray] = []
    src = 0
    for _ in range(height):
        filter_type = raw[src]
        src += 1
        row = bytearray(raw[src : src + scanline_len])
        src += scanline_len
        prev = rows[-1] if rows else bytearray(scanline_len)
        for i in range(scanline_len):
            left = row[i - bpp] if i >= bpp else 0
            up = prev[i]
            upper_left = prev[i - bpp] if i >= bpp else 0
            if filter_type == 0:
                value = row[i]
            elif filter_type == 1:
                value = (row[i] + left) & 0xFF
            elif filter_type == 2:
                value = (row[i] + up) & 0xFF
            elif filter_type == 3:
                value = (row[i] + ((left + up) // 2)) & 0xFF
            elif filter_type == 4:
                value = (row[i] + paeth(left, up, upper_left)) & 0xFF
            else:
                raise ValueError(f"unsupported PNG row filter {filter_type}")
            row[i] = value
        rows.append(row)

    rgb = bytearray(width * height * 3)
    out = 0
    for row in rows:
        if color_type == 0:
            for gray in row:
                rgb[out : out + 3] = bytes((gray, gray, gray))
                out += 3
        elif color_type == 2:
            rgb[out : out + len(row)] = row
            out += len(row)
        elif color_type == 4:
            for i in range(0, len(row), 2):
                gray = row[i]
                rgb[out : out + 3] = bytes((gray, gray, gray))
                out += 3
        elif color_type == 6:
            for i in range(0, len(row), 4):
                rgb[out : out + 3] = row[i : i + 3]
                out += 3
    return RGBImage(width, height, bytes(rgb))


def parse_png(path: Path) -> RGBImage:
    return parse_png_bytes(path.read_bytes())


def psnr(a: RGBImage, b: RGBImage) -> float:
    if a.width != b.width or a.height != b.height:
        raise ValueError(f"dimension mismatch {a.width}x{a.height} vs {b.width}x{b.height}")
    if len(a.pixels) != len(b.pixels):
        raise ValueError("pixel buffer length mismatch")
    if not a.pixels:
        raise ValueError("empty image")
    squared = 0
    for av, bv in zip(a.pixels, b.pixels):
        diff = av - bv
        squared += diff * diff
    mse = squared / len(a.pixels)
    if mse == 0:
        return math.inf
    return 20.0 * math.log10(255.0 / math.sqrt(mse))


def psnr_overlapping_crop(a: RGBImage, b: RGBImage) -> tuple[float, int, int]:
    width = min(a.width, b.width)
    height = min(a.height, b.height)
    if width <= 0 or height <= 0:
        raise ValueError("no overlapping pixels to compare")
    squared = 0
    compared = width * height * 3
    for y in range(height):
        a_row = y * a.width * 3
        b_row = y * b.width * 3
        for x in range(width * 3):
            diff = a.pixels[a_row + x] - b.pixels[b_row + x]
            squared += diff * diff
    mse = squared / compared
    if mse == 0:
        return math.inf, width, height
    return 20.0 * math.log10(255.0 / math.sqrt(mse)), width, height


def sorted_page_files(paths: list[Path]) -> list[Path]:
    def key(path: Path) -> tuple[int, str]:
        match = re.search(r"(\d+)(?=\.[^.]+$)", path.name)
        return (int(match.group(1)) if match else 0, path.name)

    return sorted(paths, key=key)


def compare_text(
    entry: dict[str, Any],
    pdf: Path,
    work_dir: Path,
    poppler: dict[str, str],
    oxide_bin: str,
    timeout: int,
) -> dict[str, Any]:
    work_dir.mkdir(parents=True, exist_ok=True)
    poppler_out = work_dir / "poppler.txt"
    oxide_out = work_dir / "oxide.txt"
    password = entry.get("password")

    poppler_cmd = [poppler["pdftotext"], "-enc", "UTF-8", "-nopgbrk"]
    if password:
        poppler_cmd.extend(["-upw", str(password)])
    poppler_cmd.extend([str(pdf), str(poppler_out)])

    oxide_cmd = [oxide_bin, "extract-text", str(pdf), "--output", str(oxide_out)]
    if password:
        oxide_cmd.extend(["--password", str(password)])

    poppler_result = run_command(poppler_cmd, timeout=timeout)
    oxide_result = run_command(oxide_cmd, timeout=timeout)
    result: dict[str, Any] = {
        "poppler": poppler_result.compact(),
        "oxide": oxide_result.compact(),
        "similarity": None,
        "poppler_chars": 0,
        "oxide_chars": 0,
    }
    if poppler_result.ok and oxide_result.ok:
        poppler_text = read_text(poppler_out)
        oxide_text = read_text(oxide_out)
        sim = text_similarity(poppler_text, oxide_text)
        result["similarity"] = sim
        result["poppler_chars"] = len(normalize_text(poppler_text))
        result["oxide_chars"] = len(normalize_text(oxide_text))
    return result


def compare_render(
    entry: dict[str, Any],
    pdf: Path,
    work_dir: Path,
    poppler: dict[str, str],
    oxide_bin: str,
    dpi: int,
    max_render_pages: int | None,
    timeout: int,
) -> dict[str, Any]:
    work_dir.mkdir(parents=True, exist_ok=True)
    poppler_prefix = work_dir / "poppler_page"
    oxide_zip = work_dir / "oxide_pages.zip"
    password = entry.get("password")

    poppler_cmd = [poppler["pdftoppm"], "-r", str(dpi)]
    if max_render_pages:
        poppler_cmd.extend(["-f", "1", "-l", str(max_render_pages)])
    if password:
        poppler_cmd.extend(["-upw", str(password)])
    poppler_cmd.extend([str(pdf), str(poppler_prefix)])

    oxide_cmd = [
        oxide_bin,
        "render",
        str(pdf),
        "--output",
        str(oxide_zip),
        "--dpi",
        str(dpi),
        "--format",
        "png",
    ]
    if max_render_pages:
        oxide_cmd.extend(["--pages", f"1-{max_render_pages}"])

    poppler_result = run_command(poppler_cmd, timeout=timeout)
    oxide_result = run_command(oxide_cmd, timeout=timeout)
    result: dict[str, Any] = {
        "poppler": poppler_result.compact(),
        "oxide": oxide_result.compact(),
        "pages": [],
        "average_psnr": None,
    }
    if not (poppler_result.ok and oxide_result.ok):
        return result

    poppler_pages = sorted_page_files(list(work_dir.glob("poppler_page-*.ppm")))
    oxide_pages: list[tuple[str, RGBImage]] = []
    try:
        with zipfile.ZipFile(oxide_zip) as archive:
            for name in sorted(archive.namelist()):
                if name.lower().endswith(".png"):
                    oxide_pages.append((name, parse_png_bytes(archive.read(name))))
    except Exception as err:  # noqa: BLE001
        result["oxide_zip_error"] = str(err)
        return result

    page_count = min(len(poppler_pages), len(oxide_pages))
    psnrs: list[float] = []
    for idx in range(page_count):
        page_result: dict[str, Any] = {
            "page_index": idx + 1,
            "poppler_image": poppler_pages[idx].name,
            "oxide_image": oxide_pages[idx][0],
            "psnr": None,
            "error": None,
        }
        try:
            poppler_img = parse_ppm(poppler_pages[idx])
            oxide_img = oxide_pages[idx][1]
            page_result["poppler_size"] = [poppler_img.width, poppler_img.height]
            page_result["oxide_size"] = [oxide_img.width, oxide_img.height]
            if poppler_img.width == oxide_img.width and poppler_img.height == oxide_img.height:
                value = psnr(poppler_img, oxide_img)
                page_result["compared_size"] = [poppler_img.width, poppler_img.height]
            else:
                value, width, height = psnr_overlapping_crop(poppler_img, oxide_img)
                page_result["dimension_mismatch"] = True
                page_result["compared_size"] = [width, height]
            page_result["psnr"] = "inf" if math.isinf(value) else round(value, 3)
            psnrs.append(100.0 if math.isinf(value) else value)
        except Exception as err:  # noqa: BLE001
            page_result["error"] = str(err)
        result["pages"].append(page_result)
    if len(poppler_pages) != len(oxide_pages):
        result["page_count_mismatch"] = {
            "poppler": len(poppler_pages),
            "oxide": len(oxide_pages),
        }
    if psnrs:
        result["average_psnr"] = round(sum(psnrs) / len(psnrs), 3)
    return result


def run_analyze(
    entry: dict[str, Any],
    pdf: Path,
    oxide_bin: str,
    timeout: int,
) -> dict[str, Any]:
    password = entry.get("password")
    cmd = [oxide_bin, "analyze", str(pdf), "--pretty"]
    if password:
        # The current CLI has no password option for analyze. Record the real behavior.
        pass
    result = run_command(cmd, timeout=timeout)
    parsed: Any = None
    if result.ok:
        try:
            parsed = json.loads(result.stdout)
        except json.JSONDecodeError:
            parsed = None
    return {"command": result.compact(), "json": parsed}


def run_extract_images(
    entry: dict[str, Any],
    pdf: Path,
    work_dir: Path,
    oxide_bin: str,
    timeout: int,
) -> dict[str, Any]:
    work_dir.mkdir(parents=True, exist_ok=True)
    output_zip = work_dir / "images.zip"
    cmd = [
        oxide_bin,
        "extract-images",
        str(pdf),
        "--output",
        str(output_zip),
        "--format",
        "original",
    ]
    result = run_command(cmd, timeout=timeout)
    count = None
    zip_error = None
    if result.ok and output_zip.exists():
        try:
            with zipfile.ZipFile(output_zip) as archive:
                count = len([n for n in archive.namelist() if not n.endswith("/")])
        except Exception as err:  # noqa: BLE001
            zip_error = str(err)
    return {"command": result.compact(), "image_count": count, "zip_error": zip_error}


def category_stats(results: list[dict[str, Any]]) -> dict[str, dict[str, Any]]:
    grouped: dict[str, list[dict[str, Any]]] = {}
    for result in results:
        grouped.setdefault(result["category"], []).append(result)
    return {category: summarize_group(items) for category, items in sorted(grouped.items())}


def summarize_group(items: list[dict[str, Any]]) -> dict[str, Any]:
    text_scores = [
        item["text"]["similarity"]["word_ratio"]
        for item in items
        if item.get("text", {}).get("similarity") is not None
    ]
    render_scores = [
        item["render"]["average_psnr"]
        for item in items
        if item.get("render", {}).get("average_psnr") is not None
    ]
    analyze_ok = [item.get("analyze", {}).get("command", {}).get("ok") for item in items]
    images_ok = [item.get("extract_images", {}).get("command", {}).get("ok") for item in items]
    return {
        "files": len(items),
        "text_files_scored": len(text_scores),
        "text_similarity": average(text_scores),
        "render_files_scored": len(render_scores),
        "render_psnr": average(render_scores),
        "analyze_success_rate": success_rate(analyze_ok),
        "extract_images_success_rate": success_rate(images_ok),
        "notes": group_notes(items),
    }


def average(values: list[float]) -> float | None:
    if not values:
        return None
    return sum(values) / len(values)


def success_rate(values: list[bool | None]) -> float | None:
    filtered = [value for value in values if value is not None]
    if not filtered:
        return None
    return sum(1 for value in filtered if value) / len(filtered)


def group_notes(items: list[dict[str, Any]], limit: int = 3) -> str:
    notes: list[str] = []
    for phase in ["text", "render", "analyze", "extract_images"]:
        failures = [
            item["id"]
            for item in items
            if not item.get(phase, {}).get("command", item.get(phase, {}).get("oxide", {})).get("ok", True)
        ]
        if failures:
            notes.append(f"{phase} failed: {', '.join(failures[:limit])}")
    return "; ".join(notes)


def fmt_percent(value: float | None) -> str:
    if value is None:
        return "n/a"
    return f"{value * 100:.1f}%"


def fmt_float(value: float | None, suffix: str = "") -> str:
    if value is None:
        return "n/a"
    return f"{value:.2f}{suffix}"


def write_outputs(
    output_dir: Path,
    manifest: dict[str, Any],
    entries: list[dict[str, Any]],
    results: list[dict[str, Any]],
    settings: dict[str, Any],
    report_path: Path | None,
) -> None:
    output_dir.mkdir(parents=True, exist_ok=True)
    stats = category_stats(results)
    overall = summarize_group(results)
    payload = {
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "settings": settings,
        "manifest": {
            key: value
            for key, value in manifest.items()
            if key not in {"entries"}
        },
        "entry_count": len(entries),
        "overall": overall,
        "categories": stats,
        "results": results,
    }
    (output_dir / "results.json").write_text(
        json.dumps(payload, indent=2, ensure_ascii=False) + "\n",
        encoding="utf-8",
    )
    write_csv(output_dir / "results.csv", results)
    summary = render_markdown_summary(payload)
    (output_dir / "summary.md").write_text(summary, encoding="utf-8")
    if report_path:
        report_path.parent.mkdir(parents=True, exist_ok=True)
        report_path.write_text(summary, encoding="utf-8")


def write_csv(path: Path, results: list[dict[str, Any]]) -> None:
    with path.open("w", newline="", encoding="utf-8") as handle:
        writer = csv.DictWriter(
            handle,
            fieldnames=[
                "id",
                "path",
                "category",
                "text_similarity",
                "text_poppler_ok",
                "text_oxide_ok",
                "render_psnr",
                "render_poppler_ok",
                "render_oxide_ok",
                "analyze_ok",
                "extract_images_ok",
                "image_count",
            ],
        )
        writer.writeheader()
        for item in results:
            similarity = item.get("text", {}).get("similarity")
            writer.writerow(
                {
                    "id": item["id"],
                    "path": item["path"],
                    "category": item["category"],
                    "text_similarity": similarity.get("word_ratio")
                    if isinstance(similarity, dict)
                    else None,
                    "text_poppler_ok": item.get("text", {}).get("poppler", {}).get("ok"),
                    "text_oxide_ok": item.get("text", {}).get("oxide", {}).get("ok"),
                    "render_psnr": item.get("render", {}).get("average_psnr"),
                    "render_poppler_ok": item.get("render", {}).get("poppler", {}).get("ok"),
                    "render_oxide_ok": item.get("render", {}).get("oxide", {}).get("ok"),
                    "analyze_ok": item.get("analyze", {}).get("command", {}).get("ok"),
                    "extract_images_ok": item.get("extract_images", {})
                    .get("command", {})
                    .get("ok"),
                    "image_count": item.get("extract_images", {}).get("image_count"),
                }
            )


def render_markdown_summary(payload: dict[str, Any]) -> str:
    settings = payload["settings"]
    overall = payload["overall"]
    lines = [
        "# Poppler Parity Baseline",
        "",
        f"Generated: {payload['generated_at']}",
        "",
        "## Scope",
        "",
        f"- Corpus files tested: {payload['entry_count']}",
        f"- DPI: {settings['dpi']}",
        f"- Render page cap: {settings['max_render_pages'] or 'all pages'}",
        f"- Poppler pdftotext: `{settings['pdftotext']}`",
        f"- Poppler pdftoppm: `{settings['pdftoppm']}`",
        f"- Oxide CLI: `{settings['oxide_bin']}`",
        "",
        "## Headline Numbers",
        "",
        f"- Overall text similarity: {fmt_percent(overall['text_similarity'])}",
        f"- Overall render PSNR: {fmt_float(overall['render_psnr'], ' dB')}",
        f"- Analyze success rate: {fmt_percent(overall['analyze_success_rate'])}",
        f"- Extract-images success rate: {fmt_percent(overall['extract_images_success_rate'])}",
        "",
        "## Category Breakdown",
        "",
        "| category | files tested | text similarity | render PSNR | extract-images success rate | notes |",
        "| --- | ---: | ---: | ---: | ---: | --- |",
    ]
    for category, stats in payload["categories"].items():
        lines.append(
            "| "
            + " | ".join(
                [
                    category,
                    str(stats["files"]),
                    fmt_percent(stats["text_similarity"]),
                    fmt_float(stats["render_psnr"], " dB"),
                    fmt_percent(stats["extract_images_success_rate"]),
                    stats["notes"].replace("|", "\\|") or "",
                ]
            )
            + " |"
        )

    worst_text = sorted(
        (
            (category, stats["text_similarity"])
            for category, stats in payload["categories"].items()
            if stats["text_similarity"] is not None
        ),
        key=lambda item: item[1],
    )[:5]
    worst_render = sorted(
        (
            (category, stats["render_psnr"])
            for category, stats in payload["categories"].items()
            if stats["render_psnr"] is not None
        ),
        key=lambda item: item[1],
    )[:5]
    lines.extend(["", "## Weakest Categories", ""])
    if worst_text:
        lines.append(
            "- Text: "
            + ", ".join(f"{category} ({score * 100:.1f}%)" for category, score in worst_text)
        )
    else:
        lines.append("- Text: no scored categories")
    if worst_render:
        lines.append(
            "- Render: "
            + ", ".join(f"{category} ({score:.2f} dB)" for category, score in worst_render)
        )
    else:
        lines.append("- Render: no scored categories")

    lines.extend(["", "## Failure Details", ""])
    failure_lines, panic_count, timeout_count = failure_details(payload["results"])
    if failure_lines:
        lines.extend(failure_lines)
    else:
        lines.append("- No command failures recorded.")
    lines.append(f"- Rust panic signatures recorded: {panic_count}")
    lines.append(f"- Command timeouts recorded: {timeout_count}")

    lines.extend(
        [
            "",
            "## Notes",
            "",
            "- Text similarity is a normalized word-token SequenceMatcher ratio against Poppler pdftotext output; very large token streams use a linear token Dice score.",
            "- Render quality is PSNR against Poppler pdftoppm PPM output. Infinite PSNR pages are capped at 100 dB for averages.",
            "- If Poppler and Oxide render dimensions differ, PSNR is computed over the overlapping crop and the mismatch is recorded per page.",
            "- A failed Oxide or Poppler command is recorded as data and does not stop the run.",
            "- The harness output directory contains results.json and results.csv with per-file command status, stderr snippets, and page-level PSNR values.",
            "",
        ]
    )
    return "\n".join(lines)


def failure_details(results: list[dict[str, Any]]) -> tuple[list[str], int, int]:
    lines: list[str] = []
    panic_count = 0
    timeout_count = 0

    def command_has_panic(command: dict[str, Any]) -> bool:
        combined = f"{command.get('stdout') or ''}\n{command.get('stderr') or ''}".lower()
        return "panic" in combined or "panicked" in combined

    def excerpt(command: dict[str, Any]) -> str:
        text = command.get("stderr") or command.get("error") or command.get("stdout") or ""
        text = " ".join(str(text).split())
        return text[:180] if text else "no stderr"

    for item in results:
        failures: list[str] = []
        text = item.get("text", {})
        for tool in ["poppler", "oxide"]:
            command = text.get(tool, {})
            if command:
                panic_count += 1 if command_has_panic(command) else 0
                timeout_count += 1 if command.get("timed_out") else 0
                if not command.get("ok"):
                    failures.append(f"text/{tool}: {excerpt(command)}")
        render = item.get("render", {})
        for tool in ["poppler", "oxide"]:
            command = render.get(tool, {})
            if command:
                panic_count += 1 if command_has_panic(command) else 0
                timeout_count += 1 if command.get("timed_out") else 0
                if not command.get("ok"):
                    failures.append(f"render/{tool}: {excerpt(command)}")
        for phase in ["analyze", "extract_images"]:
            command = item.get(phase, {}).get("command", {})
            if command:
                panic_count += 1 if command_has_panic(command) else 0
                timeout_count += 1 if command.get("timed_out") else 0
                if not command.get("ok"):
                    failures.append(f"{phase}/oxide: {excerpt(command)}")
        if failures:
            lines.append(
                f"- `{item['id']}` ({item['category']}): " + "; ".join(failures)
            )
    return lines, panic_count, timeout_count


def run_harness(args: argparse.Namespace) -> int:
    poppler_bin_dir = Path(args.poppler_bin_dir).resolve() if args.poppler_bin_dir else None
    pdftotext = find_executable("pdftotext", poppler_bin_dir)
    pdftoppm = find_executable("pdftoppm", poppler_bin_dir)
    if not pdftotext or not pdftoppm:
        install = (
            "Poppler tools are required for ground-truth comparison.\n"
            "Install poppler-utils and ensure pdftotext/pdftoppm are on PATH.\n"
            "Windows option: download the latest release from "
            "https://github.com/oschwartz10612/poppler-windows/releases and pass "
            "--poppler-bin-dir <extracted>/Library/bin.\n"
            "macOS: brew install poppler. Debian/Ubuntu: sudo apt-get install poppler-utils."
        )
        raise SystemExit(install)
    oxide_bin = resolve_oxide_bin(args)
    manifest, entries = load_entries(args)
    if not entries:
        raise SystemExit("no PDF entries matched the requested filters")

    timestamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    output_dir = Path(args.output_dir or REPO_ROOT / "target" / "poppler_compare" / timestamp)
    if not output_dir.is_absolute():
        output_dir = REPO_ROOT / output_dir
    report_path = Path(args.report_path).resolve() if args.report_path else None
    max_render_pages = None if args.max_render_pages == 0 else args.max_render_pages

    settings = {
        "dpi": args.dpi,
        "max_render_pages": max_render_pages,
        "pdftotext": pdftotext,
        "pdftoppm": pdftoppm,
        "oxide_bin": oxide_bin,
        "command_timeout": args.timeout,
        "render_timeout": args.render_timeout,
    }
    poppler = {"pdftotext": pdftotext, "pdftoppm": pdftoppm}

    results: list[dict[str, Any]] = []
    work_root = output_dir / "work"
    work_root.mkdir(parents=True, exist_ok=True)
    for index, entry in enumerate(entries, start=1):
        pdf = Path(entry["absolute_path"])
        item_id = str(entry.get("id") or pdf.stem)
        category = str(entry.get("category") or "uncategorized")
        print(f"[{index}/{len(entries)}] {item_id} ({category})", file=sys.stderr)
        file_work = work_root / sanitize(item_id)
        if file_work.exists():
            shutil.rmtree(file_work)
        file_work.mkdir(parents=True, exist_ok=True)
        if not pdf.exists():
            results.append(
                {
                    "id": item_id,
                    "path": entry.get("path"),
                    "category": category,
                    "source": entry.get("source"),
                    "error": f"PDF not found: {pdf}",
                }
            )
            continue

        result = {
            "id": item_id,
            "path": entry.get("path"),
            "category": category,
            "source": entry.get("source"),
            "license": entry.get("license"),
            "notes": entry.get("notes"),
            "size_bytes": pdf.stat().st_size,
            "text": compare_text(entry, pdf, file_work / "text", poppler, oxide_bin, args.timeout),
        }
        (file_work / "render").mkdir(parents=True, exist_ok=True)
        result["render"] = compare_render(
            entry,
            pdf,
            file_work / "render",
            poppler,
            oxide_bin,
            args.dpi,
            max_render_pages,
            args.render_timeout,
        )
        result["analyze"] = run_analyze(entry, pdf, oxide_bin, args.timeout)
        result["extract_images"] = run_extract_images(
            entry, pdf, file_work / "images", oxide_bin, args.timeout
        )
        results.append(result)

    write_outputs(output_dir, manifest, entries, results, settings, report_path)
    print(f"Wrote {output_dir / 'results.json'}", file=sys.stderr)
    print(f"Wrote {output_dir / 'summary.md'}", file=sys.stderr)
    if report_path:
        print(f"Wrote {report_path}", file=sys.stderr)
    return 0


def sanitize(value: str) -> str:
    return re.sub(r"[^A-Za-z0-9_.-]+", "_", value).strip("._") or "file"


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    input_group = parser.add_mutually_exclusive_group()
    input_group.add_argument("--manifest")
    input_group.add_argument("--input-dir")
    parser.add_argument("--category", help="Comma-separated category filter.")
    parser.add_argument("--file", help="Comma-separated id/path/name substring filter.")
    parser.add_argument("--limit", type=int)
    parser.add_argument("--output-dir")
    parser.add_argument("--report-path")
    parser.add_argument("--poppler-bin-dir")
    parser.add_argument("--oxide-bin")
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--build-timeout", type=int, default=180)
    parser.add_argument("--timeout", type=int, default=60)
    parser.add_argument("--render-timeout", type=int, default=120)
    parser.add_argument("--dpi", type=int, default=150)
    parser.add_argument(
        "--max-render-pages",
        type=int,
        default=1,
        help="Maximum pages rendered per PDF. Use 0 for all pages.",
    )
    args = parser.parse_args(argv)
    if not args.manifest and not args.input_dir:
        args.manifest = "tests/corpus/manifest.json"
    return args


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    return run_harness(args)


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
