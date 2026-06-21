#!/usr/bin/env python3
"""Classify recalibrated 0A failures into the Prompt-B input list.

Reads results/<run>/files/*.json and groups the failing pages by PRIMARY reason,
separating genuine renderer bugs from now-passing AA noise. Also lists the
fixture files for each bug so they can be saved for Prompt B.
"""
from __future__ import annotations
import json
import os
import sys
from collections import defaultdict

RUN = sys.argv[1] if len(sys.argv) > 1 else r"E:\wellpdfsdk\renderer-benchmark\results\run-0a"
FILES = os.path.join(RUN, "files")

REAL_BUG_REASONS = {
    "blank_page_mismatch",
    "rendered_page_missing",
    "large_region_difference",
    "major_color_or_inversion",
    "dimension_mismatch",
    "edge_or_text_shift",
    "perceptual_hash_distance",
}

by_reason: dict[str, list] = defaultdict(list)
files_by_reason: dict[str, set] = defaultdict(set)
normal_files = 0
normal_pass_files = 0
hostile_files = 0
hostile_crash = 0


def basename(path):
    return os.path.basename(path or "")


for fn in sorted(os.listdir(FILES)):
    if not fn.endswith(".json"):
        continue
    d = json.load(open(os.path.join(FILES, fn)))
    cat = str(d.get("category", ""))
    if cat.startswith("hostile-"):
        hostile_files += 1
        if d.get("safety", {}).get("crashed"):
            hostile_crash += 1
        continue
    normal_files += 1
    if d.get("result") == "pass":
        normal_pass_files += 1
    for fp in d["visual_compare"]["failed_pages"]:
        reason = fp.get("reason") or "unknown"
        by_reason[reason].append(
            {
                "file": basename(d.get("file")),
                "page": fp.get("page"),
                "ssim": fp.get("ssim"),
                "lr": fp.get("large_region_score"),
                "edge": fp.get("edge_mae"),
                "diag": fp.get("diagnostic_reasons"),
                "oxide_size": fp.get("oxide_size"),
                "ref_size": fp.get("reference_size"),
                "path": d.get("file"),
            }
        )
        files_by_reason[reason].add(d.get("file"))

print(f"normal files: {normal_files}, file-pass: {normal_pass_files}")
print(f"hostile files: {hostile_files}, crashes: {hostile_crash}")
print()
print("=== FAILURES BY PRIMARY REASON ===")
for reason in sorted(by_reason, key=lambda r: -len(by_reason[r])):
    tag = "REAL BUG" if reason in REAL_BUG_REASONS else "pixel/AA"
    print(f"\n[{tag}] {reason}: {len(by_reason[reason])} pages, {len(files_by_reason[reason])} files")
    for item in by_reason[reason][:60]:
        print(
            f"    {item['file']} pg{item['page']} ssim={item['ssim']} "
            f"lr={item['lr']} edge={item['edge']} oxide={item['oxide_size']} ref={item['ref_size']}"
        )

# Emit the unique fixture file paths for real-bug categories.
print("\n=== REAL-BUG FIXTURE FILES (for Prompt B) ===")
real_paths = set()
for reason, paths in files_by_reason.items():
    if reason in REAL_BUG_REASONS:
        real_paths |= paths
for p in sorted(real_paths):
    print(p)
