#!/usr/bin/env python3
"""Prompt I fidelity measurement for Oxide Compat vs High vs Poppler.

Ground truth is approximated by rendering Oxide HighQuality at 4x the target
DPI and downsampling in linear light. That intentionally measures whether the
target-DPI High mode is closer to a supersampled, gamma-correct render than the
default Compat mode and Poppler/Splash output.
"""

from __future__ import annotations

import argparse
import json
import math
import shutil
import subprocess
import zipfile
from datetime import datetime, timezone
from pathlib import Path
from typing import Iterable

from PIL import Image


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_OXIDE = ROOT / "target" / "release" / "oxide.exe"
DEFAULT_POPPLER = ROOT / "target" / "tools" / "poppler" / "poppler-26.02.0" / "Library" / "bin"


CASES = [
    {
        "name": "edge_vector_flate",
        "class": "edge/vector",
        "pdf": ROOT / "crates" / "engine" / "tests" / "fixtures" / "flate.pdf",
    },
    {
        "name": "text_basicapi",
        "class": "text-heavy",
        "pdf": ROOT / "crates" / "engine" / "tests" / "fixtures" / "basicapi.pdf",
    },
    {
        "name": "generated_vector_bars",
        "class": "vector-heavy",
        "pdf": ROOT / "tests" / "corpus" / "pdfs" / "generated" / "generated_vector_bars.pdf",
    },
    {
        "name": "synthetic_transparency",
        "class": "transparency",
        "pdf": ROOT
        / "renderer-benchmark"
        / "corpus"
        / "synthetic"
        / "synthetic_transparency_000.pdf",
    },
]


def srgb_to_linear_byte(byte: int) -> float:
    c = byte / 255.0
    if c <= 0.04045:
        return c / 12.92
    return ((c + 0.055) / 1.055) ** 2.4


SRGB_TO_LINEAR = [srgb_to_linear_byte(i) for i in range(256)]


def linear_to_srgb_byte(linear: float) -> int:
    linear = max(0.0, min(1.0, linear))
    if linear <= 0.0031308:
        s = linear * 12.92
    else:
        s = 1.055 * (linear ** (1.0 / 2.4)) - 0.055
    return int(round(max(0.0, min(1.0, s)) * 255.0))


def run(cmd: list[str], cwd: Path = ROOT) -> None:
    completed = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True)
    if completed.returncode != 0:
        raise RuntimeError(
            f"command failed ({completed.returncode}): {' '.join(cmd)}\n"
            f"stdout:\n{completed.stdout}\nstderr:\n{completed.stderr}"
        )


def load_rgb(path: Path) -> Image.Image:
    with Image.open(path) as img:
        return img.convert("RGB")


def render_oxide(oxide: Path, pdf: Path, dpi: int, mode: str, out_dir: Path, name: str) -> Image.Image:
    out_zip = out_dir / f"{name}_oxide_{mode}_{dpi}.zip"
    if out_zip.exists():
        out_zip.unlink()
    run(
        [
            str(oxide),
            "render",
            str(pdf),
            "--format",
            "png",
            "--pages",
            "1",
            "--dpi",
            str(dpi),
            "--output",
            str(out_zip),
            "--render-quality",
            mode,
        ]
    )
    with zipfile.ZipFile(out_zip) as zf:
        members = [m for m in zf.namelist() if m.lower().endswith(".png")]
        if not members:
            raise RuntimeError(f"{out_zip} did not contain a PNG")
        png_path = out_dir / f"{name}_oxide_{mode}_{dpi}.png"
        png_path.write_bytes(zf.read(members[0]))
    return load_rgb(png_path)


def render_poppler(poppler_bin: Path, pdf: Path, dpi: int, out_dir: Path, name: str) -> Image.Image:
    pdftoppm = poppler_bin / "pdftoppm.exe"
    prefix = out_dir / f"{name}_poppler_{dpi}"
    for old in out_dir.glob(f"{prefix.name}*.png"):
        old.unlink()
    run([str(pdftoppm), "-r", str(dpi), "-f", "1", "-l", "1", "-png", str(pdf), str(prefix)])
    outputs = sorted(out_dir.glob(f"{prefix.name}*.png"))
    if not outputs:
        raise RuntimeError(f"Poppler produced no PNG for {pdf}")
    return load_rgb(outputs[0])


def downsample_linear(img: Image.Image, factor: int) -> Image.Image:
    img = img.convert("RGB")
    src = img.load()
    out_w = img.width // factor
    out_h = img.height // factor
    out = Image.new("RGB", (out_w, out_h))
    dst = out.load()
    denom = float(factor * factor)
    for y in range(out_h):
        sy0 = y * factor
        for x in range(out_w):
            sx0 = x * factor
            acc = [0.0, 0.0, 0.0]
            for dy in range(factor):
                for dx in range(factor):
                    r, g, b = src[sx0 + dx, sy0 + dy]
                    acc[0] += SRGB_TO_LINEAR[r]
                    acc[1] += SRGB_TO_LINEAR[g]
                    acc[2] += SRGB_TO_LINEAR[b]
            dst[x, y] = tuple(linear_to_srgb_byte(v / denom) for v in acc)
    return out


def crop_pair(a: Image.Image, b: Image.Image) -> tuple[Image.Image, Image.Image]:
    w = min(a.width, b.width)
    h = min(a.height, b.height)
    return a.crop((0, 0, w, h)), b.crop((0, 0, w, h))


def psnr(reference: Image.Image, image: Image.Image) -> float:
    reference, image = crop_pair(reference.convert("RGB"), image.convert("RGB"))
    ref = reference.tobytes()
    got = image.tobytes()
    if not ref:
        return float("inf")
    mse = sum((ra - ga) ** 2 for ra, ga in zip(ref, got)) / len(ref)
    if mse <= 1e-12:
        return float("inf")
    return 20.0 * math.log10(255.0) - 10.0 * math.log10(mse)


def ssim(reference: Image.Image, image: Image.Image) -> float:
    reference, image = crop_pair(reference.convert("RGB"), image.convert("RGB"))
    ref = reference.tobytes()
    got = image.tobytes()
    n = len(ref) // 3
    if n == 0:
        return 1.0

    def luma_at(buf: bytes, i: int) -> float:
        r, g, b = buf[i], buf[i + 1], buf[i + 2]
        return 0.2126 * r + 0.7152 * g + 0.0722 * b

    xs = [luma_at(ref, i) for i in range(0, len(ref), 3)]
    ys = [luma_at(got, i) for i in range(0, len(got), 3)]
    mux = sum(xs) / n
    muy = sum(ys) / n
    if n == 1:
        return 1.0 if abs(mux - muy) < 1e-9 else 0.0
    varx = sum((x - mux) ** 2 for x in xs) / (n - 1)
    vary = sum((y - muy) ** 2 for y in ys) / (n - 1)
    cov = sum((x - mux) * (y - muy) for x, y in zip(xs, ys)) / (n - 1)
    c1 = (0.01 * 255.0) ** 2
    c2 = (0.03 * 255.0) ** 2
    return ((2 * mux * muy + c1) * (2 * cov + c2)) / (
        (mux * mux + muy * muy + c1) * (varx + vary + c2)
    )


def fmt_float(v: float) -> str:
    if math.isinf(v):
        return "inf"
    return f"{v:.4f}"


def summarise(rows: Iterable[dict]) -> dict:
    rows = list(rows)
    high_wins_poppler = sum(1 for r in rows if r["high_psnr"] > r["poppler_psnr"])
    high_wins_compat = sum(1 for r in rows if r["high_psnr"] > r["compat_psnr"])
    return {
        "cases": len(rows),
        "high_psnr_wins_vs_poppler": high_wins_poppler,
        "high_psnr_wins_vs_compat": high_wins_compat,
        "mean_psnr": {
            "compat": sum(r["compat_psnr"] for r in rows) / len(rows) if rows else 0.0,
            "high": sum(r["high_psnr"] for r in rows) / len(rows) if rows else 0.0,
            "poppler": sum(r["poppler_psnr"] for r in rows) / len(rows) if rows else 0.0,
        },
        "mean_ssim": {
            "compat": sum(r["compat_ssim"] for r in rows) / len(rows) if rows else 0.0,
            "high": sum(r["high_ssim"] for r in rows) / len(rows) if rows else 0.0,
            "poppler": sum(r["poppler_ssim"] for r in rows) / len(rows) if rows else 0.0,
        },
    }


def write_markdown(path: Path, payload: dict) -> None:
    lines = [
        "# Prompt I Render Fidelity Measurement",
        "",
        f"Generated: {payload['generated_at']}",
        "",
        "## Method",
        "",
        (
            f"Target renders use {payload['dpi']} DPI. Ground truth is Oxide HighQuality "
            f"at {payload['reference_dpi']} DPI, downsampled {payload['supersample']}x "
            "in linear light."
        ),
        "",
        "| case | class | Compat PSNR | High PSNR | Poppler PSNR | Compat SSIM | High SSIM | Poppler SSIM | verdict |",
        "|---|---|---:|---:|---:|---:|---:|---:|---|",
    ]
    for r in payload["results"]:
        lines.append(
            "| {name} | {class_name} | {compat_psnr} | {high_psnr} | {poppler_psnr} | "
            "{compat_ssim} | {high_ssim} | {poppler_ssim} | {verdict} |".format(
                name=r["case"],
                class_name=r["class"],
                compat_psnr=fmt_float(r["compat_psnr"]),
                high_psnr=fmt_float(r["high_psnr"]),
                poppler_psnr=fmt_float(r["poppler_psnr"]),
                compat_ssim=f"{r['compat_ssim']:.6f}",
                high_ssim=f"{r['high_ssim']:.6f}",
                poppler_ssim=f"{r['poppler_ssim']:.6f}",
                verdict=r["verdict"],
            )
        )
    s = payload["summary"]
    lines.extend(
        [
            "",
            "## Summary",
            "",
            f"- Cases measured: {s['cases']}",
            f"- High PSNR wins vs Poppler: {s['high_psnr_wins_vs_poppler']} / {s['cases']}",
            f"- High PSNR wins vs Compat: {s['high_psnr_wins_vs_compat']} / {s['cases']}",
            (
                "- Mean PSNR: Compat {compat}, High {high}, Poppler {poppler}".format(
                    compat=fmt_float(s["mean_psnr"]["compat"]),
                    high=fmt_float(s["mean_psnr"]["high"]),
                    poppler=fmt_float(s["mean_psnr"]["poppler"]),
                )
            ),
            (
                "- Mean SSIM: Compat {compat:.6f}, High {high:.6f}, Poppler {poppler:.6f}".format(
                    compat=s["mean_ssim"]["compat"],
                    high=s["mean_ssim"]["high"],
                    poppler=s["mean_ssim"]["poppler"],
                )
            ),
            "",
            payload["verdict"],
            "",
        ]
    )
    path.write_text("\n".join(lines), encoding="utf-8")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--oxide-bin", type=Path, default=DEFAULT_OXIDE)
    parser.add_argument("--poppler-bin-dir", type=Path, default=DEFAULT_POPPLER)
    parser.add_argument("--dpi", type=int, default=72)
    parser.add_argument("--supersample", type=int, default=4)
    parser.add_argument("--work-dir", type=Path, default=ROOT / "target" / "render_quality_prompt_i")
    parser.add_argument("--json", type=Path, default=ROOT / "docs" / "render_quality_prompt_i_results.json")
    parser.add_argument("--markdown", type=Path, default=ROOT / "docs" / "render_quality_prompt_i_summary.md")
    args = parser.parse_args()

    if not args.oxide_bin.exists():
        raise SystemExit(f"missing Oxide binary: {args.oxide_bin}")
    if not (args.poppler_bin_dir / "pdftoppm.exe").exists():
        raise SystemExit(f"missing pdftoppm.exe under {args.poppler_bin_dir}")

    if args.work_dir.exists():
        shutil.rmtree(args.work_dir)
    args.work_dir.mkdir(parents=True, exist_ok=True)

    results = []
    reference_dpi = args.dpi * args.supersample
    for case in CASES:
        pdf = case["pdf"]
        if not pdf.exists():
            continue
        name = case["name"]
        compat = render_oxide(args.oxide_bin, pdf, args.dpi, "compat", args.work_dir, name)
        high = render_oxide(args.oxide_bin, pdf, args.dpi, "high", args.work_dir, name)
        high_ref = render_oxide(args.oxide_bin, pdf, reference_dpi, "high", args.work_dir, f"{name}_ref")
        reference = downsample_linear(high_ref, args.supersample)
        ref_path = args.work_dir / f"{name}_reference_linear_downsample.png"
        reference.save(ref_path)
        poppler = render_poppler(args.poppler_bin_dir, pdf, args.dpi, args.work_dir, name)

        row = {
            "case": name,
            "class": case["class"],
            "pdf": str(pdf.relative_to(ROOT)),
            "compat_psnr": psnr(reference, compat),
            "high_psnr": psnr(reference, high),
            "poppler_psnr": psnr(reference, poppler),
            "compat_ssim": ssim(reference, compat),
            "high_ssim": ssim(reference, high),
            "poppler_ssim": ssim(reference, poppler),
        }
        if row["high_psnr"] > row["poppler_psnr"] and row["high_psnr"] > row["compat_psnr"]:
            row["verdict"] = "High wins"
        elif row["high_psnr"] > row["compat_psnr"]:
            row["verdict"] = "High beats Compat only"
        elif row["high_psnr"] > row["poppler_psnr"]:
            row["verdict"] = "High beats Poppler only"
        else:
            row["verdict"] = "High does not win"
        results.append(row)

    summary = summarise(results)
    if summary["cases"] and summary["high_psnr_wins_vs_poppler"] == summary["cases"]:
        verdict = "Measured verdict: HighQuality beats Poppler on PSNR for every measured case."
    elif summary["cases"] and summary["high_psnr_wins_vs_poppler"] > 0:
        verdict = (
            "Measured verdict: HighQuality improves some cases, but the fidelity win is "
            "not universal on this fixture set."
        )
    else:
        verdict = (
            "Measured verdict: HighQuality did not beat Poppler on this fixture set; "
            "treat it as a technically different display mode, not a proven fidelity win."
        )

    payload = {
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "oxide_bin": str(args.oxide_bin),
        "poppler_bin_dir": str(args.poppler_bin_dir),
        "dpi": args.dpi,
        "supersample": args.supersample,
        "reference_dpi": reference_dpi,
        "results": results,
        "summary": summary,
        "verdict": verdict,
    }
    args.json.parent.mkdir(parents=True, exist_ok=True)
    args.json.write_text(json.dumps(payload, indent=2), encoding="utf-8")
    write_markdown(args.markdown, payload)
    print(json.dumps(summary, indent=2))
    print(verdict)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
