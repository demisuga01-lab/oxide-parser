import importlib.util
import pathlib
import sys
import unittest


SCRIPT_PATH = pathlib.Path(__file__).resolve().parents[1] / "scripts" / "renderer_benchmark.py"
SPEC = importlib.util.spec_from_file_location("renderer_benchmark", SCRIPT_PATH)
renderer_benchmark = importlib.util.module_from_spec(SPEC)
assert SPEC and SPEC.loader
sys.modules["renderer_benchmark"] = renderer_benchmark
SPEC.loader.exec_module(renderer_benchmark)


class RendererThresholdTests(unittest.TestCase):
    def test_phash_only_antialias_noise_is_diagnostic(self):
        metrics = {
            "phash_distance": 16,
            "different_pixel_percent": 0.2442,
            "mae": 0.1744,
            "edge_mae": 0.001853,
            "large_region_score": 0.055556,
            "blank_score_oxide": 0.051904,
            "blank_score_reference": 0.052597,
            "ssim": 0.937922,
        }

        thresholds = renderer_benchmark.thresholds("renderer", text_heavy=True)
        self.assertTrue(renderer_benchmark.is_phash_aa_noise(metrics, thresholds))

    def test_phash_with_content_drift_stays_structural(self):
        metrics = {
            "phash_distance": 18,
            "different_pixel_percent": 1.1952,
            "mae": 0.8221,
            "edge_mae": 0.040294,
            "large_region_score": 0.296296,
            "blank_score_oxide": 0.05,
            "blank_score_reference": 0.05,
            "ssim": 0.909914,
        }

        thresholds = renderer_benchmark.thresholds("renderer", text_heavy=True)
        self.assertFalse(renderer_benchmark.is_phash_aa_noise(metrics, thresholds))

    def test_low_energy_gradient_pixel_noise_is_diagnostic(self):
        metrics = {
            "different_pixel_percent": 12.4146,
            "mae": 0.2413,
            "ssim": 0.999985,
            "edge_mae": 0.001044,
            "large_region_score": 0.0,
            "blank_score_oxide": 0.05,
            "blank_score_reference": 0.05,
            "max_channel_delta": 8,
        }

        thresholds = renderer_benchmark.thresholds("renderer", text_heavy=False)
        self.assertTrue(renderer_benchmark.is_low_energy_pixel_noise(metrics, thresholds))

    def test_pixel_difference_with_text_drift_stays_structural(self):
        metrics = {
            "different_pixel_percent": 12.1234,
            "mae": 5.7001,
            "ssim": 0.900304,
            "edge_mae": 0.060141,
            "large_region_score": 0.246914,
            "blank_score_oxide": 0.05,
            "blank_score_reference": 0.05,
            "max_channel_delta": 255,
        }

        thresholds = renderer_benchmark.thresholds("renderer", text_heavy=True)
        self.assertFalse(renderer_benchmark.is_low_energy_pixel_noise(metrics, thresholds))

    def test_large_region_text_antialias_noise_is_diagnostic(self):
        metrics = {
            "different_pixel_percent": 9.7565,
            "mae": 1.2041,
            "ssim": 0.964814,
            "edge_mae": 0.010124,
            "large_region_score": 0.578,
            "blank_score_oxide": 0.04,
            "blank_score_reference": 0.041,
            "phash_distance": 6,
            "max_channel_delta": 255,
        }

        thresholds = renderer_benchmark.thresholds("renderer", text_heavy=True)
        self.assertTrue(renderer_benchmark.is_large_region_text_aa_noise(metrics, thresholds))
        self.assertTrue(renderer_benchmark.is_low_energy_pixel_noise(metrics, thresholds, True))

    def test_large_region_with_structural_drift_stays_structural(self):
        metrics = {
            "different_pixel_percent": 12.1234,
            "mae": 5.7001,
            "ssim": 0.900304,
            "edge_mae": 0.060141,
            "large_region_score": 0.578,
            "blank_score_oxide": 0.05,
            "blank_score_reference": 0.05,
            "phash_distance": 22,
            "max_channel_delta": 255,
        }

        thresholds = renderer_benchmark.thresholds("renderer", text_heavy=True)
        self.assertFalse(renderer_benchmark.is_large_region_text_aa_noise(metrics, thresholds))


if __name__ == "__main__":
    unittest.main()
