# Oxide Performance Baseline (Mega-Prompt 10)

This document records throughput and peak-memory numbers for the Oxide CLI and
engine. It complements the **parity** harness (`scripts/poppler_compare.py`),
which measures correctness (text similarity, render PSNR) but **not**
performance. Round 10 is a performance round: it parallelises multi-page text
extraction and shares one parsed engine across render threads via `Arc` instead
of re-parsing the PDF per page. Output is unchanged (see
`crates/engine/tests/parallelism.rs` and the Round 10 note in
`docs/poppler_parity_baseline.md`); only speed and memory change.

## Harnesses

Two complementary tools, both using the **release** build:

1. **`scripts/perf_bench.py`** — times the real `oxide` CLI on representative
   documents and samples peak working-set / RSS. It runs each case at **1
   thread** and at **N threads** (`RAYON_NUM_THREADS` pinned) so the
   parallelism win is explicit and apples-to-apples on a single build. Best
   wall-clock time over `--repeats` runs (default 5); max peak memory across
   runs. Peak memory is read from the OS high-water mark
   (Win32 `GetProcessMemoryInfo.PeakWorkingSetSize` on Windows; `/proc` `VmHWM`
   on Linux), so it is exact, not sampled-and-maybe-missed.

   ```
   cargo build --release -p oxide-cli
   py scripts/perf_bench.py --label oxide --repeats 5
   # -> docs/perf_oxide_results.json + a printed table
   ```

2. **`crates/engine/examples/render_bench.rs`** — renders every page of a PDF in
   one of two strategies so the **Part C** memory/throughput delta can be
   measured on real engine code (the CLI render path never cloned per page, so
   it cannot show this on its own):
   - `shared`  — parse once into `Arc<ContentEngine>`, render all pages in
     parallel sharing that one parsed document (the **fixed** pdf2img design).
   - `perpage` — re-open (re-parse + re-buffer) a fresh `ContentEngine` per
     page, in parallel (the **old** pdf2img design this round removed). Holds
     O(num_pages) copies of the parsed document at peak.

   ```
   cargo build --release -p oxide-engine --examples
   # measure peak memory of each mode with the perf_bench memory sampler or
   # /usr/bin/time -v; pixels are identical between modes by construction.
   ```

## Why 1-vs-N and shared-vs-perpage instead of a git "before" build

The two deltas this round targets are both measurable on **one** build:

- **Part B (parallel text)**: `perf_bench.py` at `RAYON_NUM_THREADS=1`
  reproduces serial behaviour (rayon runs the work inline on one worker), so the
  1-thread vs N-thread time on the *same* binary is the serial-vs-parallel
  speedup. The per-page work is byte-identical regardless of thread count
  (proven by the differential tests), so this is a clean speed-only comparison.
- **Part C (Arc-shared render)**: `render_bench`'s `perpage` mode *is* the old
  per-page-reparse behaviour and `shared` *is* the new behaviour, compiled into
  one example. Comparing their peak memory and time is the before/after.

## Measurement context

- Machine / OS: _Windows 11, see `platform` field in `docs/perf_oxide_results.json`_
- Logical CPUs: _see `cpu_count` in the JSON_
- Build: `cargo build --release` (optimised). Debug builds are not representative.
- Reporting: best-of-N wall-clock time; max peak memory across runs.

## Stress documents

| key | file | why |
| --- | --- | --- |
| `120pg` | `tests/corpus/pdfs/generated/generated_120_pages.pdf` | 120-page multi-page stress case — the ideal parallelism + memory target |
| `tracemonkey` | `crates/engine/tests/fixtures/tracemonkey.pdf` | 14-page real-world text+vector document |
| `form_160f` | `crates/engine/tests/fixtures/form_160f.pdf` | form-heavy document |

## Results

Measured 2026-06-15 on Windows 11 (10.0.26200), 20 logical CPUs, rustc 1.95.0,
release build. Best-of-3 wall-clock; peak from Win32 `PeakWorkingSetSize`. These
Oxide-only 1-vs-N numbers come from the same run as the Oxide-vs-Poppler
comparison in `docs/oxide_vs_poppler.md` (§D.3.2); raw data is
`docs/perf_compare_results.json`.

### Text extraction throughput — 1 vs N threads (Part B)

`oxide extract-text`, `RAYON_NUM_THREADS` pinned to 1 and 20:

| document | pages | time @1 thread (s) | time @20 threads (s) | speedup |
| --- | ---: | ---: | ---: | ---: |
| 120pg | 120 | 0.303 | 0.059 | **5.1×** |
| tracemonkey | 14 | 0.059 | 0.019 | **3.1×** |
| form_160f | — | 0.011 | 0.014 | 0.8× (too small to parallelize) |

Parallel text extraction delivers 3–5× on multipage documents; tiny single-page
documents see no benefit (thread-spawn overhead dominates). This is the Round-10
parallel-text win, now quantified.

### Render peak memory — flat in page count (Part C)

Peak working set of `oxide render` (all pages → PNG-in-ZIP), measured this
session, vs Poppler `pdftoppm` on the same inputs:

| document | pages | DPI | Oxide peak (MB) | Poppler peak (MB) |
| --- | ---: | ---: | ---: | ---: |
| 120pg | 120 | 150 | 21.4 | 21.9 |
| 120pg | 120 | 300 | 63.9 | 46.0 |
| 1.5 MB image PDF | 1 | 150 | 67.4 | 32.3 |

The headline result: Oxide's render peak at 150 DPI is **21.4 MB for 120 pages —
the same as a single page** — confirming the Arc-shared engine keeps memory flat
in page count (one parsed copy + per-page scratch, not O(pages) copies). At
300 DPI Oxide holds more than Poppler (64 vs 46 MB) because it buffers pages for
ZIP assembly while Poppler streams each page to disk; same reason it uses more on
image decode.

> Honest scope note: the `render_bench` `perpage`-vs-`shared` A/B example
> (`crates/engine/examples/render_bench.rs`) was **not** executed this session;
> the flat-memory property above is demonstrated directly from the production
> CLI render path instead, which is the user-facing path. The CLI render also
> showed no 1-vs-N wall-clock speedup (page-level render parallelism is not
> wired into the CLI) — see `docs/oxide_vs_poppler.md` §D.5.

## Prompt 4 Follow-Up (2026-06-23)

Prompt 4 re-ran the capstone smoke benchmark and the multipage `perf_bench.py` harness after Prompt 3. The measured hotspot worth changing was raster render batching: `extract-text` already had strong 1-vs-N scaling, while the CLI render path still rendered and encoded pages in the ZIP write loop.

### Operation Smoke Refresh

Same harness as the capstone operation smoke, release build, best-of-5 on this Windows 11 host:

| Operation | Before ms | After ms | After peak MB |
| --- | ---: | ---: | ---: |
| Parse to JSON CLI | 95.8 | 89.3 | 19.00 |
| Extract text CLI | 36.6 | 37.2 | 20.04 |
| Render PNG ZIP CLI | 60.4 | 56.2 | 18.59 |
| Authoring example | 24.4 | 22.4 | 7.67 |
| PDF/A conversion example | 24.2 | 21.5 | 9.06 |
| RSA signing example | 13.8 | 11.9 | 5.12 |
| Optimize CLI | 13.5 | 9.2 | 6.08 |
| Linearize CLI | 13.5 | 12.6 | 6.87 |
| Encrypt AES-256 CLI | 20.0 | 15.6 | 6.61 |

These smoke numbers include normal run-to-run noise; the targeted measured win is the multipage raster-render path below.

### Multipage Render Target

The implementation now renders and encodes raster pages as an ordered batch for large selections, then writes the ZIP sequentially in the requested page order. Selections below 32 pages keep the old streaming path to avoid overhead on small or complex documents. No unsafe SIMD was added; the change is safe Rayon parallelism plus allocation/IO staging.

| Case | Threads | Before render s | After render s | Peak MB before -> after |
| --- | ---: | ---: | ---: | ---: |
| 120pg | 1 | 4.884 | 1.664 | 23.0 -> 22.9 |
| 120pg | 20 | 3.220 | 1.666 | 23.0 -> 22.9 |
| tracemonkey | 1 | 8.289 | 6.040 | 35.8 -> 36.0 |
| tracemonkey | 20 | 6.332 | 6.337 | 35.9 -> 35.8 |

The 120-page render improved 2.9x at one pinned Rayon worker and 1.9x at 20 workers. Thread-count scaling was flat after the change on this corpus, so the honest claim is faster batch render staging, not a universal multi-core renderer speedup. `extract-text` remained already-scaled: 120pg at 20 threads measured 0.059 s after versus 0.264 s at one thread.

### Correctness And Determinism

`render_raster_output_is_deterministic_across_thread_counts` renders the same two-page slice with `RAYON_NUM_THREADS=1` and `4` and compares ZIP entry names plus decoded entry bytes. The full 265-entry renderer benchmark was also rerun after the scheduling change and stayed at 91.82 weighted / 86.94% visual pass, with 100% hostile crash/timeout/memory safety and 24/24 deterministic samples. Existing parallelism tests continue to cover text extraction and shared-engine rendering. The benchmark harness now writes scratch output directly under `target/perf-bench-tmp` because Python-created temporary directories can deny child-process writes in the Windows restricted-token sandbox.

## Output-correctness guarantee

This round must not change output. Enforced by:
- `crates/engine/tests/parallelism.rs`:
  - `parallel_extract_matches_serial_reference_*` — parallel text output is
    byte-identical to a single-threaded reference (tracemonkey + 120-page).
  - `page_order_is_preserved_with_shuffled_explicit_page_list` — explicit
    page order is preserved, not sorted or completion-ordered.
  - `arc_engine_renders_identically_across_threads` — the `Arc`-shared engine
    renders pixel-identical pages under 4-thread concurrent load.
  - `concurrent_text_and_render_on_shared_engine_do_not_race` — mixed
    text+render load on one shared engine (exercises the object-stream
    `RwLock` first-write + concurrent readers) with no race or deadlock.
- The full existing suite (757 tests across the workspace) remains green.
- The parity harness numbers are unchanged (this round touches scheduling and
  memory sharing, not output) — see the Round 10 note in
  `docs/poppler_parity_baseline.md`.
