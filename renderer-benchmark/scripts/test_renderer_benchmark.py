import unittest
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))

import renderer_benchmark


class RendererBenchmarkThresholdTests(unittest.TestCase):
    def test_large_files_use_text_heavy_thresholds(self) -> None:
        self.assertTrue(renderer_benchmark.is_text_heavy("large-files"))
        self.assertEqual(
            renderer_benchmark.thresholds("renderer", text_heavy=True)["different_pixel_percent"],
            8.0,
        )

    def test_synthetic_graphics_use_text_heavy_pixel_threshold_only(self) -> None:
        self.assertTrue(renderer_benchmark.is_text_heavy("synthetic-graphics"))
        text_thresholds = renderer_benchmark.thresholds("renderer", text_heavy=True)
        flat_thresholds = renderer_benchmark.thresholds("renderer", text_heavy=False)
        self.assertEqual(text_thresholds["different_pixel_percent"], 8.0)
        self.assertEqual(text_thresholds["large_region"], flat_thresholds["large_region"])
        self.assertEqual(text_thresholds["blank_delta"], flat_thresholds["blank_delta"])

    def test_hostile_visual_gaps_do_not_pollute_fidelity_breakdowns(self) -> None:
        hostile = {
            "category": "hostile-huge-page",
            "fail_reasons": [],
            "visual_compare": {
                "failed_pages": [
                    {
                        "reason": "rendered_page_missing",
                        "reasons": ["rendered_page_missing"],
                        "page": 1,
                    }
                ]
            },
        }
        normal = {
            "category": "real-pdfjs-general",
            "file": "normal.pdf",
            "fail_reasons": ["blank_page_mismatch"],
            "visual_compare": {
                "failed_pages": [
                    {
                        "reason": "blank_page_mismatch",
                        "reasons": ["blank_page_mismatch"],
                        "page": 1,
                    }
                ]
            },
        }

        self.assertEqual(
            renderer_benchmark.failure_breakdown([hostile, normal]),
            {"blank_page_mismatch": 1, "page:blank_page_mismatch": 1},
        )
        self.assertEqual(len(renderer_benchmark.blocking_findings([hostile, normal])), 2)


if __name__ == "__main__":
    unittest.main()
