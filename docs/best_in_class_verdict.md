# Best-in-Class Verdict — Measurement & Checkpoint (Prompt 6)

This is a **measurement + integration + verdict checkpoint**, not a positioning
rewrite and not a new-feature plan. It records what was *freshly verified this
session* against the final commit, what was *not re-run and why*, and an honest
verdict about where Oxide stands and what the genuine next step is.

> **Scope decision for this checkpoint.** The best-in-class positioning push
> (Prompts 1–5) is **held**: it improved the README front door, scanned-OCR
> extraction, renderer fidelity, multipage render performance, and key-material
> zeroization — but it did **not** close the High-severity findings from the
> project's own internal audit (`docs/security/audit_findings.md`). Per the
> Prompt-6 decision, **no outward-facing security positioning is strengthened
> here.** The recommended next step is a dedicated *fix-the-Highs* batch
> **before** any best-in-class positioning is published. See §5.

---

## 1. Provenance

| Item | Value |
| --- | --- |
| Final HEAD at measurement | `8673875` (`feat: harden key material and sanitizer coverage`) |
| Date | 2026-06-24 |
| Host | Windows 11, this session |
| Toolchain | `cargo 1.95.0`, `rustc 1.95.0` |
| Comparators present | PyMuPDF 1.27.2.3, Poppler `pdftotext`/`pdftocairo` 26.02.0, qpdf, Tesseract 5.x |
| Comparators **absent** (could not be re-run) | **PDFium**, **veraPDF**, **Docling**, mutool |

Every number below is tagged **[fresh]** (measured this session against the
final tree) or **[recorded]** (carried from a prior session's artifact, with the
reason it was not re-run). Nothing is fabricated. Absent tools are reported as
absent, not estimated.

---

## 2. Integration — does everything still compose? **[fresh] — GREEN**

| Gate | Command | Result |
| --- | --- | --- |
| Full test suite | `cargo test --workspace --no-fail-fast` | **1,380 passed / 0 failed**, exit 0 (48 test binaries + 5 doc-test sections) |
| Lint gate | `cargo clippy --workspace --all-targets -- -D warnings` | **0 warnings / 0 errors**, exit 0 |
| Cross-feature compose (fresh, via CLI + qpdf) | `optimize`, `linearize`, `encrypt`, `merge` | all **qpdf-clean** (see §3.3) |

The extraction, renderer-calibration, performance, and hardening changes from
Prompts 1–5 did not break composition: the workspace builds, the whole test
suite passes, and clippy is clean under `-D warnings`. (Note: the parallelism
and surface-consistency integration tests are part of the 1,380 and pass,
covering the byte-identical-across-surfaces and deterministic-parallel-extract
invariants.)

---

## 3. Fresh benchmarks

### 3.1 Extraction quality **[fresh]** — vs PyMuPDF, Poppler `pdftotext`

Scored with the in-repo pure-Rust `oxide eval-score` metrics over the
ground-truth corpus. Competitors detected at runtime; **Docling absent → cleanly
skipped, no Docling number reported.** Char-accuracy = `1 − CER`; reading order =
normalized Kendall-tau.

| Doc | Mode | Oxide char-acc | PyMuPDF | pdftotext | Oxide reading-order |
| --- | --- | ---: | ---: | ---: | ---: |
| figure | digital | 0.598 | **0.990** | 0.931 | 1.000 |
| paper | digital | 0.993 | **0.998** | 0.951 | 1.000 |
| report_multicol | digital | 0.605 | **0.669** | 0.347 | **1.000** |
| tables | digital | **1.000** | 0.877 | 0.298 | 1.000 |
| paper_scanned | scanned | **0.942** | 0.000 | 0.000 | 1.000 |
| tables_scanned | scanned | **0.649** | 0.000 | 0.000 | 1.000 |

Structure (cell-F1 / TEDS, and KV field-F1 — capabilities PyMuPDF/Poppler do not
offer):

| Doc | Mode | Oxide cell-F1 / TEDS | PyMuPDF cell-F1 | Oxide field-F1 |
| --- | --- | ---: | ---: | ---: |
| invoice | digital | 1.000 / 1.000 | 0.000 | **1.000** |
| tables | digital | 1.000 / 1.000 | 1.000 | — |
| receipt | digital | — | — | 0.800 |
| invoice_scanned | scanned | 1.000 / 1.000 | 0.000 | **0.857** |
| tables_scanned | scanned | 1.000 / 1.000 | 0.000 | — |

**Honest reading of these numbers:**
- Oxide's real edge is **structure**: perfect reading-order, table cell-F1/TEDS,
  and KV fields — things the comparators don't do at all.
- On **raw character accuracy of plain/graphic text**, Oxide **trails** PyMuPDF
  (figure 0.598 vs 0.990; multi-column 0.605 vs 0.669) and only ties on clean
  prose (paper 0.993 ≈ 0.998). "Leads on digital extraction" is true for
  *structure*, **not** for raw text fidelity.
- On **scans**, Oxide recovers content where PyMuPDF/Poppler recover **nothing**.
  The scanned numbers **improved** from the figures still printed in the README
  (invoice_scanned field-F1 **0.400 → 0.857**; scanned-table cell-F1 the README
  calls "0" now scores 1.000 *on the synthetic fixture*). The README extraction
  table is therefore **stale and understated** on scans and should be refreshed
  when positioning is unheld.
- **Corpus caveat:** one synthetic fixture per category. The perfect scanned
  structure scores (1.000) are on synthetic scans, **not** messy real-world
  documents; do not read them as a general scanned-table guarantee.

### 3.2 Renderer fidelity **[recorded — not re-swept this session]**

Source: `renderer-benchmark/results/prompt3-final-265/aggregate.md`, generated
2026-06-23T17:17Z (Prompt 3). **Not re-run here** because the 265-file sweep
needs the release binary + a Poppler reference pass, and **PDFium is absent**
(so the pixel-reference comparison cannot be improved on this host — it was
already PDFium-absent / Poppler-only when recorded).

- Weighted score **91.82** (up from the ~86.12 pre-Prompt-3 baseline), visual
  pass **86.94%**, hostile crash/timeout/memory-safety **100%/100%/100%**,
  determinism 24/24, median Poppler/Oxide speed ratio 1.91×.
- Weakest categories (honest): RTL 40%, scanned 44%, multi-column 47%, forms
  57%, CJK 62%. The renderer is **preview/OCR-grade, not pixel-proof**, and
  trails Poppler/PDFium for visual fidelity. The report itself carries a
  "not a Tier-3 claim at this corpus scale" caveat.

### 3.3 Structural / compliance cross-checks **[fresh]**

Generated by the OCR-enabled debug CLI this session and validated with external
qpdf:

| Operation | External check | Result |
| --- | --- | --- |
| `optimize` | `qpdf --check` | Clean — "No syntax or stream encoding errors found" |
| `linearize` | `qpdf --check` | "File is linearized", clean |
| `encrypt --user-pw` | `qpdf --check --password` | R=6, AESv3 (stream/string/file), opens + clean |
| `merge a a` | `oxide info` + `qpdf --check` | 2 pages (1+1), clean |

### 3.4 Performance **[recorded — not re-run this session]**

Source: `docs/perf_prompt_h_summary.md` (release binary, median-of-3) plus the
Prompt-4 multipage-render perf artifacts. **Not re-run** here (would require a
release rebuild + the cold-start/footprint harness). The honest summary:

- **Decisive wins (footprint/latency):** ~7.5 ms process cold start vs ~158 ms
  for Python+PyMuPDF import; ~12.8 MB static binary vs a Python+C-extension
  stack; lower per-op peak memory; faster on `info`, single-page text, and
  image extraction (image extraction ~10× Poppler in the recorded run).
- **Losses (be honest):** Oxide is **slower than Poppler** on *full-document
  rendering* (render_all ratios ≈ 0.47–0.81×) and on *large-document full-text
  extraction* (≈ 0.10–0.59×). A blanket "fastest-in-class" claim is **not**
  supported by the numbers; "smallest footprint / fastest cold start / faster on
  common single-doc ops" **is**.

### 3.5 Compliance & hostile safety **[recorded]**

- PDF/A-1b/2b/2a/3b/3a: **veraPDF 1.30.2 PASS** — recorded in the capstone
  (2026-06-22). **veraPDF is absent on this host**, so this was not re-verified
  this session.
- Cross-pillar hostile sweep: 265 files / 1,590 ops / **0 crashes / 0 timeouts**
  — recorded (`posture.md`, hardening consolidation 2026-06-23). The 60 hostile
  files in the renderer sweep (§3.2) re-confirm 100% crash/timeout safety.

---

## 4. Security reality (the part that gates the verdict)

The project's own systematic audit (`docs/security/audit_findings.md`) reported
**0 Critical · 6 High · 8 Medium · 12 Low/Info**. Re-checked against the final
tree this session, **all 6 High findings are still open**; Prompts 1–5 closed
exactly **one Low** (L-4 key-material zeroization, now real via
`SecretBytes = Zeroizing<Vec<u8>>` and `.zeroize()` throughout `crypto.rs`).

| ID | High finding | Status in final tree | Evidence |
| --- | --- | --- | --- |
| H-1 | Redaction under-removes text (fixed-width fake glyph metrics → text leaks under the black box) | **OPEN** | `editing.rs` `advance_text` = `bytes*500.0`; `glyph_rect` = `font_size*0.5` |
| H-2 | Redaction does not scrub alternate text reps (`/ActualText`, struct tree, XMP, attachments) | **OPEN** | no `BDC`/struct-tree/XMP handling in `editing.rs` |
| H-3 | Signature `Valid` ≠ trusted (no chain/validity/KeyUsage/revocation gating) | **OPEN** | `signature.rs` `scope_note`: "…are not verdict gates" |
| H-4 | Image bit-depth normalize allocates from unbounded PDF dims (OOM) | **OPEN** | decode layer has no pixel cap; `saturating_mul` added but no ceiling |
| H-5 | CCITT sink pre-allocates `columns×rows` (OOM) | **OPEN** | `ccitt.rs` `Vec::with_capacity(width*height)`, uncapped |
| H-6 | JBIG2 sink pre-allocates from codestream dims (OOM) | **OPEN** | `jbig2.rs` `Vec::with_capacity(width*height)`, uncapped |

Two of these directly contradict current outward-facing claims:
- **H-1 vs the README feature line "redaction (with extract-back verification)".**
  The core `RedactionReport` only tracks removed-text strings to drive metadata
  scrubbing — there is no runtime "extract back and assert the secret is gone"
  guarantee, and the glyph geometry is unreliable for proportional/CID fonts.
  This claim should be tempered (it is part of the fix-the-Highs work).
- **H-4/5/6 vs "rendering has DPI and pixel caps."** The pixel cap is in the
  *render* layer; the *decode* layer (reachable from `extract-images`, `pdf2img`,
  and the server) has no cap. A few-hundred-byte PDF can force a multi-GB
  allocation.

`posture.md`, the README, and the capstone verdict currently frame the only
security residual as "external audit + pilot." That is **incomplete**: there are
concrete, code-level, self-identified Highs that an external audit is **not**
needed to find and that should be fixed first. `audit_findings.md` is committed
as the tracked known-issues record, and `posture.md` now points to it.

---

## 5. Verdict

### Where Oxide is genuinely best-in-class (on its real strengths)
- **Pure-Rust, memory-safe, single static binary, self-hostable, four embed
  surfaces, permissive (MIT/Apache-2.0), one canonical `Document` model** — this
  breadth-in-one-safe-core with no Python/C++ runtime is a real, defensible
  differentiator.
- **Footprint & cold start**: ~12.8 MB / ~7.5 ms beats a Python+PyMuPDF stack
  decisively.
- **Digital structured extraction**: reading-order, table cell-F1/TEDS, and KV
  fields at 1.000 on the corpus — capabilities the text-dump comparators don't
  have.
- **Scan recovery via OCR** where PyMuPDF/Poppler recover nothing — and
  **improved** this cycle.

### Where it is competitive
- Clean-prose digital text accuracy (≈ PyMuPDF), `info`/single-page/image-extract
  speed (faster than Poppler), PDF/A compliance (veraPDF PASS, recorded),
  structural ops (qpdf-clean, fresh).

### Where it trails
- **Raw text char-accuracy** on figure/graphic-heavy and multi-column pages
  (loses chars to PyMuPDF while winning order).
- **Full-document render & large-doc full-text speed** (slower than Poppler).
- **Renderer visual fidelity** (preview-grade; trails Poppler/PDFium).
- **Messy real-world scans / ML layout** (Docling not benchmarked locally; likely
  ahead on the hardest scans).
- **Security**: 6 open self-identified High findings — **currently blocking** a
  best-in-class security claim and a clean GA tag.

### Release readiness
**Not ready to strengthen best-in-class positioning, and not ready for a strict
v1.0 GA tag.** It remains a strong **pilot / release-candidate** engine on its
non-security axes. The gating work is, in order:

1. **Fix the 6 High findings** (code — a dedicated batch; this is NOT positioning
   or docs work). Redaction true-removal + alt-text scrub, a shared decode-layer
   pixel cap, and signature trust-semantics (chain/validity/revocation gating or
   an unambiguous type-level `cryptographically_valid` vs `trusted`).
2. **Then** commission the third-party security audit.
3. **Then** run a real pilot.

### Honest closing
The code is in a strong, well-tested, well-hardened state, and integration holds
green. Prompts 1–5 delivered real, measured improvements (README clarity,
scanned extraction, renderer fidelity, multipage perf, key zeroization). But the
honest verdict is bounded by the project's own audit: **you cannot call a thing
best-in-class on security while six of your own High findings are open.** The
genuine next step is therefore **not** a seventh batch of positioning prompts —
it is a focused **fix-the-Highs** code batch, and then the **external audit + a
real pilot**. That is where "best" gets earned from here.
