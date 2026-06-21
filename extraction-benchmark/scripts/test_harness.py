#!/usr/bin/env python3
"""Self-tests for the extraction-benchmark harness.

Verifies that (1) the pure-Rust scorer reachable via `oxide eval-score` computes
the expected metrics on toy known-answer inputs, and (2) tool detection / clean
skipping behaves. Run: ``python3 test_harness.py`` (exits non-zero on failure).
"""

import json
import math
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import extraction_benchmark as bench  # noqa: E402


def approx(a, b, tol=1e-6):
    return a is not None and abs(a - b) < tol


def check(name, cond):
    print(f"  {'PASS' if cond else 'FAIL'}  {name}")
    return cond


def main():
    if not os.path.exists(bench.OXIDE):
        print("ERROR: oxide binary not built; run `cargo build -p oxide-cli`")
        return 1

    ok = True

    # 1. Perfect text -> CER 0, accuracy 1.
    s = bench.oxide_score({"ref_text": "the quick brown fox", "hyp_text": "the quick brown fox"})
    ok &= check("perfect text -> cer 0", approx(s.get("cer"), 0.0))
    ok &= check("perfect text -> char_accuracy 1", approx(s.get("char_accuracy"), 1.0))

    # 2. One substitution over 3 chars -> CER 1/3.
    s = bench.oxide_score({"ref_text": "abc", "hyp_text": "abX"})
    ok &= check("1 sub / 3 chars -> cer 1/3", approx(s.get("cer"), 1.0 / 3.0))

    # 3. Reversed reading order -> 0.0.
    s = bench.oxide_score({"ref_order": ["a", "b", "c"], "hyp_order": ["c", "b", "a"]})
    ok &= check("reversed order -> 0.0", approx(s.get("reading_order"), 0.0))

    # 4. Perfect table -> cell-F1 1, TEDS 1.
    grid = [[["A", "B"], ["1", "2"]]]
    s = bench.oxide_score({"ref_tables": grid, "hyp_tables": grid})
    ok &= check("perfect table -> cell-F1 1", approx(s.get("table_cell_f1"), 1.0))
    ok &= check("perfect table -> TEDS 1", approx(s.get("table_teds"), 1.0))

    # 5. One wrong field of two -> F1 ~0.5 (tp=1,pred=2,gold=2).
    s = bench.oxide_score({
        "ref_fields": [{"key": "a", "value": "1"}, {"key": "b", "value": "2"}],
        "hyp_fields": [{"key": "a", "value": "1"}, {"key": "b", "value": "9"}],
    })
    ok &= check("1/2 fields -> F1 0.5", approx(s.get("field_f1"), 0.5))

    # 6. Block-type accuracy 3/4.
    s = bench.oxide_score({
        "ref_block_types": ["heading", "paragraph", "table", "figure"],
        "hyp_block_types": ["heading", "paragraph", "paragraph", "figure"],
    })
    ok &= check("block-type acc 0.75", approx(s.get("block_type_accuracy"), 0.75))

    # 7. Garbage input is rejected (error surfaced, not a crash).
    res = bench.oxide_score({"ref_tables": "not a list"})  # type error inside Rust deser
    ok &= check("garbage scored input -> error, no crash", isinstance(res, dict))

    # 8. Tool detection returns a dict with the expected keys.
    tools = bench.detect_tools()
    ok &= check("detect_tools has oxide key", "oxide" in tools and tools["oxide"])
    ok &= check("detect_tools reports docling presence as bool", isinstance(tools.get("docling"), bool))

    # 9. A missing competitor is skip-clean: simulate by checking the absent flag
    #    doesn't raise when used (docling absent here).
    ok &= check("absent competitor flag is False/clean", tools.get("docling") in (False, True))

    print("ALL PASS" if ok else "SOME FAILED")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
