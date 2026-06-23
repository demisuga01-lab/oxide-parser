#!/usr/bin/env python3
"""Private-CI cargo-fuzz runner.

This script keeps the GitHub Actions workflow small and gives developers the
same commands locally. It deliberately runs the out-of-workspace `fuzz/` crate
and does not affect normal stable builds.
"""

from __future__ import annotations

import argparse
import subprocess
import sys
from pathlib import Path


REPO = Path(__file__).resolve().parents[1]
FUZZ = REPO / "fuzz"
DEFAULT_TARGETS = [
    "parse_pdf",
    "filters",
    "predictor",
    "content_tokenizer",
    "image_decoders",
    "fonts",
    "cmap",
    "crypto",
    "functions",
    "writer",
    "document_rewrite",
    "linearize",
    "pdfa",
    "editing",
    "signature_validation",
    "structured_pdf",
]


def run(cmd: list[str], *, cwd: Path = FUZZ) -> None:
    print("+", " ".join(cmd), flush=True)
    completed = subprocess.run(cmd, cwd=cwd, check=False)
    if completed.returncode != 0:
        raise SystemExit(completed.returncode)


def has_seed(corpus_dir: Path) -> bool:
    return corpus_dir.exists() and any(path.is_file() for path in corpus_dir.rglob("*"))


def artifact_prefix(target: str) -> str:
    path = REPO / "target" / "ci-fuzz-artifacts" / target
    path.mkdir(parents=True, exist_ok=True)
    return f"{path.as_posix()}/"


def parse_targets(raw: str) -> list[str]:
    if raw == "all":
        return DEFAULT_TARGETS
    targets = [item.strip() for item in raw.split(",") if item.strip()]
    unknown = sorted(set(targets) - set(DEFAULT_TARGETS))
    if unknown:
        raise SystemExit(f"unknown fuzz target(s): {', '.join(unknown)}")
    return targets


def build_target(target: str) -> None:
    run(["cargo", "+nightly", "fuzz", "build", target])


def replay_regressions(target: str) -> None:
    corpus = FUZZ / "corpus" / target
    if not has_seed(corpus):
        print(f"skip {target}: no committed regression/seed corpus", flush=True)
        return
    run(
        [
            "cargo",
            "+nightly",
            "fuzz",
            "run",
            target,
            f"corpus/{target}",
            "--",
            "-runs=0",
            f"-artifact_prefix={artifact_prefix(target)}",
        ]
    )


def timed_fuzz(target: str, seconds: int, max_len: int) -> None:
    (FUZZ / "corpus" / target).mkdir(parents=True, exist_ok=True)
    run(
        [
            "cargo",
            "+nightly",
            "fuzz",
            "run",
            target,
            "--",
            f"-max_total_time={seconds}",
            f"-max_len={max_len}",
            f"-artifact_prefix={artifact_prefix(target)}",
        ]
    )


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--targets", default="all", help="'all' or comma-separated target names")
    parser.add_argument(
        "--mode",
        choices=["build", "regression", "smoke", "deep"],
        required=True,
    )
    parser.add_argument("--seconds", type=int, default=45)
    parser.add_argument("--max-len", type=int, default=65536)
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--print-targets", action="store_true")
    args = parser.parse_args()

    targets = parse_targets(args.targets)
    if args.print_targets:
        print("\n".join(targets))
        return

    for target in targets:
        print(f"== {target} ({args.mode}) ==", flush=True)
        if not args.no_build:
            build_target(target)
        if args.mode == "build":
            continue
        if args.mode == "regression":
            replay_regressions(target)
        else:
            timed_fuzz(target, args.seconds, args.max_len)


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        sys.exit(130)
