# Prompt I Render Fidelity Measurement

Generated: 2026-06-17T13:40:38.499406+00:00

## Method

Target renders use 72 DPI. Ground truth is Oxide HighQuality at 288 DPI, downsampled 4x in linear light.

| case | class | Compat PSNR | High PSNR | Poppler PSNR | Compat SSIM | High SSIM | Poppler SSIM | verdict |
|---|---|---:|---:|---:|---:|---:|---:|---|
| edge_vector_flate | edge/vector | 53.3042 | 95.4431 | 46.0286 | 0.995168 | 1.000000 | 0.973023 | High wins |
| text_basicapi | text-heavy | 31.4638 | 36.6344 | 27.8589 | 0.847826 | 0.933434 | 0.725153 | High wins |
| generated_vector_bars | vector-heavy | 30.6559 | 37.6589 | 27.1209 | 0.996495 | 0.999280 | 0.992138 | High wins |
| synthetic_transparency | transparency | 28.9395 | 51.4891 | 20.7636 | 0.865704 | 0.998682 | 0.472733 | High wins |

## Summary

- Cases measured: 4
- High PSNR wins vs Poppler: 4 / 4
- High PSNR wins vs Compat: 4 / 4
- Mean PSNR: Compat 36.0909, High 55.3064, Poppler 30.4430
- Mean SSIM: Compat 0.926298, High 0.982849, Poppler 0.790762

Measured verdict: HighQuality beats Poppler on PSNR for every measured case.
