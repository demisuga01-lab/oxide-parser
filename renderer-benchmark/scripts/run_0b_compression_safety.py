#!/usr/bin/env python3
"""Benchmark 0B scaffold: Oxide(original) vs Oxide(compressed).

This is intentionally separate from 0A. It proves that a compression step did
not visually alter a file, not that Oxide matches Poppler/PDFium.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(Path(__file__).resolve().parent))
from renderer_benchmark import (  # noqa: E402
    compare_page_sets,
    executable_name,
    render_oxide,
)


def discover_pairs(root: Path) -> list[dict[str, Path]]:
    pairs: list[dict[str, Path]] = []
    for case_dir in sorted(p for p in root.iterdir() if p.is_dir()):
        original = case_dir / "original.pdf"
        compressed = case_dir / "compressed.pdf"
        if original.exists() and compressed.exists():
            pairs.append({"id": case_dir.name, "original": original, "compressed": compressed})
    before_files = sorted(root.glob("*.before.pdf"))
    for before in before_files:
        after = before.with_name(before.name.replace(".before.pdf", ".after.pdf"))
        if after.exists():
            pairs.append({"id": before.name.replace(".before.pdf", ""), "original": before, "compressed": after})
    return pairs


def safe_id(value: str) -> str:
    return re.sub(r"[^A-Za-z0-9_.-]+", "_", value)[:120]


def entry(path: Path, case_id: str, side: str) -> dict[str, Any]:
    return {
        "id": f"{case_id}_{side}",
        "path": str(path),
        "absolute_path": str(path),
        "category": "wellpdf-0b",
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--oxide-bin", default=str(REPO_ROOT / "target" / "release" / executable_name("oxide")))
    parser.add_argument("--pairs-dir", default=str(REPO_ROOT / "renderer-benchmark" / "corpus" / "wellpdf-before-after"))
    parser.add_argument("--output-dir", default=str(REPO_ROOT / "renderer-benchmark" / "results" / "run-0b"))
    parser.add_argument("--dpi", type=int, default=144)
    parser.add_argument("--timeout-sec", type=int, default=20)
    parser.add_argument("--max-memory-mb", type=int, default=1024)
    parser.add_argument("--max-pages-per-file", type=int)
    args = parser.parse_args()

    pairs_dir = Path(args.pairs_dir)
    output_dir = Path(args.output_dir)
    output_dir.mkdir(parents=True, exist_ok=True)
    (output_dir / "artifacts").mkdir(exist_ok=True)
    pairs = discover_pairs(pairs_dir) if pairs_dir.exists() else []
    results: list[dict[str, Any]] = []
    for pair in pairs:
        case_id = safe_id(pair["id"])
        work_dir = output_dir / "artifacts" / case_id
        original = render_oxide(
            entry(pair["original"], case_id, "original"),
            args.oxide_bin,
            work_dir / "original",
            args.dpi,
            args.max_pages_per_file,
            args.timeout_sec,
            args.max_memory_mb,
            suffix="original",
        )
        compressed = render_oxide(
            entry(pair["compressed"], case_id, "compressed"),
            args.oxide_bin,
            work_dir / "compressed",
            args.dpi,
            args.max_pages_per_file,
            args.timeout_sec,
            args.max_memory_mb,
            suffix="compressed",
        )
        visual = compare_page_sets(compressed["pages"], original["pages"], "compression")
        result = {
            "id": case_id,
            "original": str(pair["original"]),
            "compressed": str(pair["compressed"]),
            "render": {
                "original_success": original["command"]["ok"],
                "compressed_success": compressed["command"]["ok"],
                "original": original["command"],
                "compressed": compressed["command"],
            },
            "visual_compare": visual,
            "result": "pass" if original["command"]["ok"] and compressed["command"]["ok"] and not visual["failed_pages"] else "fail",
        }
        results.append(result)
        (output_dir / f"{case_id}.json").write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")

    aggregate = {
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "mode": "0B-compression-safety",
        "pairs_dir": str(pairs_dir),
        "pairs": len(pairs),
        "passed": sum(1 for r in results if r["result"] == "pass"),
        "failed": sum(1 for r in results if r["result"] == "fail"),
        "note": "0B is separate from 0A and uses stricter same-renderer thresholds.",
        "results": results,
    }
    (output_dir / "aggregate.json").write_text(json.dumps(aggregate, indent=2) + "\n", encoding="utf-8")
    (output_dir / "aggregate.md").write_text(
        "# Benchmark 0B Compression Safety\n\n"
        f"Pairs: {aggregate['pairs']}\n\n"
        f"Passed: {aggregate['passed']}\n\n"
        f"Failed: {aggregate['failed']}\n\n"
        "This scaffold does not contribute to 0A renderer-compatibility scoring.\n",
        encoding="utf-8",
    )
    print(f"Wrote {output_dir / 'aggregate.json'}")


if __name__ == "__main__":
    main()
