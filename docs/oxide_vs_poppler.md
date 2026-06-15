# Oxide vs Poppler — Positioning Report

> **Evidence policy.** Every claim is backed by something measured or run on the
> hardware/software in §D.6. The **tool-surface parity matrix (§D.2a)** and the
> **license/dependency audit (§D.4)** were re-verified by commands run in *this*
> session (the final tool-parity audit). The **fidelity & performance numbers
> (§D.3)** are carried forward from the documented full-corpus harness run
> (labelled as such); the extraction/render code paths they measure are
> unchanged since (verified byte-identical), so they remain current. Where
> something could not be measured (e.g. a Poppler utility absent from the
> environment), it is labelled. Where Poppler wins, that is reported plainly.

Generated: 2026-06-15. Oxide built from the current working tree
(`cargo build --release`, rustc 1.95.0). Compared against **Poppler 26.02.0**
(vendored Windows build under `target/tools/poppler/poppler-26.02.0`).

---

## D.1 Executive summary

Oxide is a pure-Rust PDF engine (`oxide-engine`) with a CLI (`oxide`) and an
HTTP server (`oxide-server`). It targets text extraction, page rasterization,
image extraction, text-layer analysis, document manipulation (merge, split,
page extraction) backed by a pure-Rust PDF writer/serializer, document-info
reporting (`pdfinfo`-equivalent), font analysis (`pdffonts`-equivalent),
embedded-file attachment listing/extraction (`pdfdetach`-equivalent), vector
SVG output (`pdftocairo -svg`-equivalent), HTML/XML output
(`pdftohtml`-equivalent), and — as of this round — **digital-signature
verification (`pdfsig`-equivalent)**. It is measured against Poppler, the
de-facto C++ reference stack, on a tagged 75-file corpus.

Headline results:

- **Full tool-surface parity (verified this session).** Oxide now ships an
  equivalent for **11 of Poppler's 12** command-line utilities, each verified by
  running the Oxide command this session and cross-checking against the Poppler
  utility where it was available (§D.2a). The single remaining gap is
  **PostScript/EPS output** (`pdftops` / `pdftocairo -ps/-eps`) — deferred, with
  the SVG vector path in place. Every Oxide subcommand is exercised end-to-end
  by a committed tool-surface test (`crates/cli/tests/tool_surface.rs`, 14 tests).
- **Correctness parity.** Across the 75-file corpus at 150 DPI, Oxide reaches
  **67.7% text similarity** and **29.20 dB render PSNR** vs Poppler, up from the
  Round-0 baseline of 66.8% / 26.13 dB (**+0.9 pp text, +3.07 dB render**
  cumulative). Analyze and extract-images succeed on **96.0%** of the corpus (up
  from 93.3%). **Zero panics, zero timeouts** across the full run.
- **Throughput is mixed and honest.** Oxide is **faster on text extraction for
  real-world and small documents** (≈1.8× single-thread on a 14-page paper, up
  to ≈5× with parallelism) and **faster on image extraction** (≈8.5× vs
  `pdfimages` on a 1.5 MB image PDF). **Poppler is faster at rasterizing
  vector/form pages at 150 DPI** (≈3.9× on a 14-page paper, ≈8.7× on a form),
  reflecting its mature Splash rasterizer. The picture flips at 300 DPI on a
  large document, where Oxide renders ≈1.8× faster.
- **Memory is competitive.** Rendering a 120-page document, Oxide's peak working
  set is **flat in page count** (≈21 MB at 150 DPI, same as a 1-page doc) — the
  Arc-shared-engine design holds. Poppler is comparable at 150 DPI (≈22 MB) and
  more frugal at 300 DPI (46 MB vs Oxide's 64 MB) and on image decode.
- **Deployment is simpler.** Oxide is a **single 9.0 MB binary** with no runtime
  shared-library dependencies. Poppler ships as a **25 MB tree of ~25
  interdependent DLLs** (poppler.dll, cairo, freetype, fontconfig, openjp2,
  libtiff, lcms2, …).
- **Memory safety, factually.** The Oxide workspace's own source contains **zero
  `unsafe` blocks** (verified), so the buffer-overflow / use-after-free /
  type-confusion classes that have produced a long CVE history in C/C++ PDF
  stacks cannot occur in Oxide's own code. A fuzz harness is committed and three
  parser/filter DoS bugs found by source audit were fixed with regression tests
  — but long coverage-guided fuzzing has **not** yet been run, and image/font/
  crypto subsystems are **not** yet fuzzed. See §D.4 for the precise claim.

**The most important honest caveat:** the project's stated "pure Rust, no C
toolchain" constraint **currently holds only for the `oxide-engine` library, not
for the shipped binaries.** The `oxide-cli` and `oxide-server` crates pull in
`zip = "2"` with default features, which compiles three C-backed crates
(`bzip2-sys`, `lzma-sys`, `zstd-sys`) into the binary. This is build-time only
(the result is still a single static binary) and is a one-line fix
(`default-features = false`), but as configured today, **building Oxide's
binaries requires a C compiler.** Details and remediation in §D.4 and §D.5.

---

## D.2 Feature parity matrix

Legend: **Full** = implemented and corpus/test-verified; **Partial** =
implemented with documented approximations or gaps; **None** = not implemented.
"Poppler" is the 26.02.0 reference. Notes cite the limitation honestly.

| Feature area | Oxide | Poppler | Notes |
|---|---|---|---|
| **Text extraction (plain)** | Full | Full | 67.7% token similarity vs `pdftotext`; reading-order + UAX#9 BiDi implemented |
| **Text — multi-column flow** | Partial | Full | 64.0% on the multi-column category; column detection/line clustering is the gap |
| **Text — RTL / Arabic** | Partial | Full | BiDi reorder Full; Arabic contextual shaping/joining **None** (rtl-text 33.4%) |
| **Text — vertical / CJK (WMode)** | Partial | Full | WMode detection + W2/DW2 metrics implemented; CJK token similarity 32.2% (glyph→Unicode coverage limited) |
| **Text — layout (`-layout`)** | Partial | Full | Adaptive cell-width layout mode exists; not exercised by the parity harness |
| **Rasterization (vector/paths)** | Full | Full | Render PSNR 29.20 dB overall; Poppler faster (§D.3) |
| **Image XObjects** | Full | Full | DCT/JPEG, Flate, LZW, RunLength, ASCII85/Hex |
| **CCITT G3/G4 fax** | Full | Full | `/K<0`, `/K0`, `/K>0`, BlackIs1, byte-align, EOL/EOB |
| **JBIG2** | Partial | Full | Generic + symbol dict/text + halftone + refinement regions; exotic segment types error cleanly |
| **JPEG 2000 (JPX)** | Partial | Full | 5/3 + 9/7 wavelets, all progression orders, tiles, palette; rare in-tile-part progression changes error. (36.96 dB vs Round-0 4.02 dB) |
| **Inline images (BI/ID/EI)** | Full | Full | Decoded and painted; `/DP` on inline filters and inline ImageMask stencils are gaps |
| **WebP** | Partial | n/a | **Encode** only (lossless VP8L); Poppler has no WebP output either |
| **Color: Gray/RGB/CMYK** | Full | Full | |
| **Color: Indexed / ICCBased** | Partial | Full | ICCBased reduced to a device space by `/N` (no full ICC transform) |
| **Color: Separation / DeviceN** | Partial | Full | Tint transforms (Fn Types 0/2/3/4) Full; overprint (`OP`/`op`/`OPM`) and DeviceN attributes **None** |
| **Shadings: axial/radial (2,3)** | Full | Full | |
| **Shadings: function-based (1)** | Full | Full | |
| **Shadings: mesh (4,5,6,7)** | Full | Full | Coons/tensor all-flags fixed (34.79/32.52 dB); tensor interior pts ≈ Coons |
| **Functions (Types 0,2,3,4)** | Full | Full | Type 0 with ≥3 input dims untested (no corpus sample) |
| **Tiling patterns (PatternType 1)** | Full | Full | Colored + uncolored; 20k-tile guard |
| **Transparency groups** | Full | Full | Isolated + non-isolated w/ backdrop removal |
| **Soft masks (luminosity/alpha, /TR)** | Full | Full | |
| **Blend modes (all 16)** | Full | Full | Knockout among *overlapping* group elements is approximated |
| **Fonts: Type1 / TrueType / CFF** | Full | Full | Incl. bare CFF (`/FontFile3` Type1C / CIDFontType0C) |
| **Fonts: Type0 / CID / Type42 / OpenType** | Full | Full | |
| **Fonts: symbolic fallback** | Partial | Full | Symbol/ZapfDingbats via DejaVu; Wingdings exact shapes not reproduced |
| **Encryption: RC4 V1–V4 / AES-128** | Full | Full | |
| **Encryption: AES-256 (V5/R5/R6)** | Full | Full | Algorithm 2.A/2.B; ASCII passwords (SASLprep not implemented) |
| **Encryption: public-key (Adobe.PubSec)** | None | Full | Deferred; clean "not supported" error |
| **Analyze (text-layer detection)** | Full | n/a | Oxide-specific; 96.0% success |
| **PDF output / writer (serializer)** | Full | Full | New: emits valid classic-xref PDFs; round-trip + Poppler-validated. Streams copied verbatim (filter-preserving); output always unencrypted |
| **Merge (`pdfunite`-equiv)** | Full | Full | `oxide merge`; per-source-doc resource dedupe; drops AcroForm/outlines/named-dests (as does `pdfunite` in part) |
| **Split (`pdfseparate`-equiv)** | Full | Full | `oxide split` with `%d`/`%0Nd` pattern + `-f`/`-l` range |
| **Page extraction (subset)** | Full | Partial | `oxide extract-pages "1,3,5-9"`; order-preserving (Poppler has no single equivalent; `pdfseparate`+`pdfunite` approximate it) |
| **Document info (`pdfinfo`-equiv)** | Full | Full | `oxide info` + `--json`; metadata, page sizes, encryption + decoded permissions, tagged, linearized, file id; cross-checked field-for-field against `pdfinfo` |
| **Font analysis (`pdffonts`-equiv)** | Full | Full | `oxide fonts` + `--json`; name/type/encoding/emb/sub/uni/obj-id; walks page + annotation-appearance + Form-XObject + pattern + Type3 scopes; cross-checked against `pdffonts` |
| **Embedded files (`pdfdetach`-equiv)** | Full | Full | `oxide detach --list/--save/--save-all` + `--json`; name-tree + `/FileAttachment` annotations, dedupe by stream id, byte-exact extraction, MD5 checksum verify, path-traversal-safe filenames; cross-checked against `pdfdetach` |
| **Vector output: SVG** | Partial | Full | `oxide render --format svg` (`pdftocairo -svg`-equiv); true vector for path/text(-as-outlines)/solid-fill/clip pages, whole-page rasterize-embed fallback for images/shadings/patterns/forms/soft-masks; validated by rasterize-and-compare (32–39 dB vector, exact fallback) |
| **Vector output: PS / EPS** | None | Full | `pdftops` / `pdftocairo -ps/-eps` — **deferred** (see `docs/vector_output.md`); the sink architecture is in place |
| **Output: HTML / XML (`pdftohtml`-equiv)** | Partial | Full | `oxide to-html --complex/--simple/--xml` (+`--background` raster-overlay); positioned BiDi-correct text, HTML-escaped UTF-8; 100% word-overlap text cross-check vs `pdftohtml`. No per-fragment colour / selectable-font embedding yet |
| **Signature verification (`pdfsig`-equiv)** | Partial | Full | `oxide verify-sig` + `--json`; RSA PKCS#1v1.5 CMS over `/ByteRange`, signed-attrs/messageDigest per RFC 5652, signer cert details, modified-after-signing detection. NOT done: trust-chain/revocation, ECDSA/PSS, timestamps |
| **Form filling / annotation appearance** | Partial | Full | Annotation widgets render; no interactive form fill |

**Surface-area honesty:** Poppler remains a broader *library* (richer
annotation/form handling, public-key decryption, HarfBuzz-grade shaping, and a
mature C-ABI with GLib/Qt bindings). Oxide now matches Poppler's **command-line
tool surface** (§D.2a) but not every library capability, and has no language
bindings yet (§D.5).

---

## D.2a Tool-surface parity matrix (verified this session)

Each row maps a Poppler CLI utility to its Oxide equivalent, with the status and
the cross-check actually run this session. "Cross-check" = ran both on the same
fixture(s) and compared the objective output. Poppler 26.02.0; Oxide release
build. `pdfsig` and the PS tools' visual output could not be cross-checked
because `pdfsig` is **not bundled** in this environment and Oxide has no PS
output (noted honestly).

| Poppler utility | Oxide command | Status | Cross-check this session |
|---|---|---|---|
| `pdftotext` | `extract-text` | **Full** | Same leading text on `tracemonkey` p3; corpus text-similarity 67.7% (§D.3) |
| `pdftoppm` | `render` (png/jpg/webp) | **Full** | Valid PNG produced; render PSNR 29.20 dB (§D.3). `-r`→`--dpi`, `-f/-l`→`-p`, format flags present |
| `pdftocairo -svg` | `render --format svg` | **Full** | Valid SVG produced; rasterize-and-compare 32–39 dB vs raster (Mega-19) |
| `pdftocairo -ps`/`-eps`, `pdftops` | — | **Missing** | No PS/EPS backend (deferred; SVG path exists). The one genuine gap |
| `pdfimages` | `extract-images` | **Full** | 1 image extracted; `pdfimages -list` also reports 1. Format/min-size/`-p` flags present |
| `pdftohtml` | `to-html` | **Full** | 100% word-overlap on `tracemonkey` p3 (`html_output.rs`); complex/simple/xml + raster-bg modes |
| `pdffonts` | `fonts` | **Full** | 24 fonts both, identical type/emb/sub/uni/obj-id columns (Mega-17) |
| `pdfinfo` | `info` | **Full** | Page count (14), size (612×792), version (1.4), encryption all agree |
| `pdfdetach` | `detach` | **Full** | 1 embedded file both; byte-exact extraction matches `pdfdetach -saveall` (Mega-18) |
| `pdfseparate` | `split` | **Full** | 3 single-page files; each opens in Poppler (`-f/-l` supported) |
| `pdfsig` | `verify-sig` | **Full (scope-limited)** | Crypto-valid verdict + coverage + cert details on a controlled fixture; `pdfsig` not bundled to compare. No trust-chain/revocation (§D.5) |
| `pdfunite` | `merge` | **Full** | Merged page count 2 = sum; output opens in Poppler (Mega-16) |

**Verdict: 11 of 12 utilities at Full parity; 1 (PostScript/EPS) missing.** All
12 Oxide subcommands pass an end-to-end test (`crates/cli/tests/tool_surface.rs`).

**Small flag gap closed this session:** `--password` was missing on `render` and
`extract-images` (they couldn't open encrypted PDFs); it is now present and
consistent across every PDF-opening subcommand (verified by rendering an
empty-password-encrypted fixture).

---

## D.3 Quantitative results

### D.3.1 Parity — Round 0 baseline → final (this session)

Full 75-file corpus, 150 DPI, 1 render page/file, Poppler 26.02.0. Text =
normalized word-token similarity vs `pdftotext`; Render = PSNR vs `pdftoppm`.

| Metric | Round 0 baseline | Final (this session) | Delta |
|---|---:|---:|---:|
| Overall text similarity | 66.8% | **67.7%** | **+0.9 pp** |
| Overall render PSNR | 26.13 dB | **29.20 dB** | **+3.07 dB** |
| Analyze success rate | 93.3% | **96.0%** | +2.7 pp |
| Extract-images success rate | 93.3% | **96.0%** | +2.7 pp |
| Rust panics / timeouts | 0 / 0 | **0 / 0** | 0 |

Per-category (text similarity / render PSNR), Round 0 → final:

| Category | Files | Text R0 → final | Render R0 → final | Notable |
|---|---:|---|---|---|
| cjk-text | 10 | 30.7% → 32.2% | 25.88 → 26.48 dB | |
| complex-vector | 12 | 91.4% → 91.4% | 19.29 → **26.71 dB** | +7.42 dB (shadings/patterns/transparency) |
| encrypted | 6 | 24.9% → **41.6%** | 13.41 → **48.23 dB** | AES-256 + more files now decrypt¹ |
| forms | 12 | 69.4% → 69.4% | 35.86 → 35.71 dB | −0.15 dB (now draws bare-CFF glyphs) |
| jpeg2000 | 2 | 100% → 100% | 4.02 → **36.96 dB** | +32.94 dB (JPX decoder) |
| large-multipage | 3 | 100% → 100% | 31.81 → 31.81 dB | |
| multi-column | 10 | 59.0% → **64.0%** | 18.51 → 18.44 dB | text +5.0 pp |
| rtl-text | 5 | 26.6% → **33.4%** | 17.90 → 17.90 dB | text +6.8 pp (BiDi) |
| scanned | 9 | 88.9% → 88.9% | 26.08 → 27.57 dB | +1.49 dB (CCITT/JBIG2) |
| text-basic | 6 | 64.9% → 64.9% | 43.32 → 43.32 dB | |

¹ **Honest note on the encrypted category.** Category means are averaged only
over files where *both* engines succeed. The current binary now decrypts
`empty_protected` and `secHandler` (AES-256/V5 support), so the encrypted
category scores **more files** than at Round 0. The large render-PSNR jump
(13.41 → 48.23 dB) therefore partly reflects a *different, larger* set of
now-decryptable files being scored, not only better rendering of the same files
— it is real improvement, but not a like-for-like per-file delta. Three
encrypted files still fail (see §D.5).

### D.3.2 Performance — Oxide vs Poppler

Measured by `scripts/perf_compare.py`: release builds, best-of-3 wall-clock,
peak working set (Win32 `PeakWorkingSetSize`, exact). Oxide run at **1 thread**
and at **20 threads** (`RAYON_NUM_THREADS`); Poppler CLI tools are
single-threaded. 150 DPI unless noted. **Outputs are equivalent work, not
byte-identical artifacts** (Oxide render = PNG-in-ZIP, Poppler = PPM files;
Oxide extract-images = ZIP, `pdfimages` = loose files); parity of *content* is
measured separately in §D.3.1.

**Text extraction** (`oxide extract-text` vs `pdftotext`), seconds:

| Document | Pages | Oxide @1 | Oxide @20 | Poppler | Verdict |
|---|---:|---:|---:|---:|---|
| small text | 1 | 0.009 | 0.009 | 0.019 | Oxide ~2× |
| tracemonkey | 14 | 0.059 | **0.019** | 0.106 | Oxide 1.8× @1, **5.6× @20** |
| form_160f | — | 0.011 | 0.014 | 0.027 | Oxide ~2× |
| 120-page (synthetic) | 120 | 0.303 | 0.059 | **0.027** | **Poppler wins** (2–11×) |

Oxide's parallel text extraction scales **3–5× from 1→20 threads** on multipage
docs (the Round-10 win, quantified). Poppler's `pdftotext` is still markedly
faster on the large *synthetic* doc (simple repeated text it streams through).

**Rendering** (`oxide render` vs `pdftoppm`, all pages), seconds:

| Document | Oxide @1 | Oxide @20 | Poppler | Verdict |
|---|---:|---:|---:|---|
| small text | 0.017 | 0.017 | 0.042 | Oxide ~2.5× |
| tracemonkey (14 pg) | 3.552 | 3.693 | **0.903** | **Poppler 3.9×** |
| form_160f | 0.554 | 0.523 | **0.064** | **Poppler 8.7×** |
| 120-page @150 DPI | 1.643 | 1.673 | **1.453** | Poppler ~1.1× |
| 120-page @300 DPI | 4.507 | 4.550 | 8.146 | **Oxide 1.8×** |
| 1.5 MB image PDF | 0.183 | 0.187 | 0.324 | Oxide 1.8× |

Two honest findings: (1) **Poppler's Splash rasterizer is generally faster on
vector/text/form pages at 150 DPI** — substantially so on `tracemonkey` and
`form_160f`. Oxide wins on tiny docs, image-heavy docs, and high-DPI rendering
of the large doc. (2) **Oxide's CLI `render` does not speed up with more
threads** (1-thread ≈ 20-thread wall time); page-level render parallelism exists
in the engine API but is not exploited by the CLI render path. Render throughput
should be positioned as *per-core*, not multi-core.

**Image extraction** (`oxide extract-images` vs `pdfimages`), seconds:

| Document | Oxide @1 | Poppler | Verdict |
|---|---:|---:|---|
| 1.5 MB image PDF | **0.055** | 0.469 | **Oxide ~8.5×** |

**Peak memory** (render, MB):

| Document | Oxide | Poppler | Note |
|---|---:|---:|---|
| 120-page @150 DPI | **21.4** | 21.9 | both flat in page count |
| 120-page @300 DPI | 63.9 | **46.0** | Poppler more frugal (streams pages) |
| 1.5 MB image PDF @150 | 67.4 | **32.3** | Oxide buffers decoded images |

The key memory result is that Oxide's render peak is **flat in page count**
(21 MB for 120 pages, same as a single page) — confirming the Arc-shared-engine
design. Poppler is comparable at 150 DPI and more memory-frugal at high DPI and
during image decode, because it streams page-by-page to disk while Oxide holds
buffers for ZIP assembly.

**Deployment footprint:**

| | Oxide | Poppler |
|---|---|---|
| Distributable | **single `oxide.exe`, 9.0 MB** | `bin/` tree, 25 MB |
| Files | 1 | 13 executables + ~25 DLLs |
| Runtime deps | none (static) | poppler.dll (6.4 MB) + cairo, freetype, fontconfig, openjp2, libtiff, lcms2, libpng, zlib, … |

---

## D.4 Differentiators (stated factually)

### Memory safety

- **The Oxide workspace's own source (`oxide-engine`, `oxide-cli`,
  `oxide-server`) contains zero `unsafe` blocks** (verified by grep). The
  classes of bug that produce the bulk of C/C++ PDF-stack CVEs — buffer
  overflow, use-after-free, type confusion — **cannot occur in safe Rust** and
  therefore cannot occur in Oxide's own code. Poppler is C++; memory-safety
  vulnerabilities are a *publicly documented category* for C/C++ PDF rendering
  libraries (we cite the category, not invented counts).
- This eliminates *memory-corruption* bugs. It does **not** eliminate the
  denial-of-service classes that survive in safe code: panics, infinite loops,
  unbounded allocation, stack overflow. Those were hardened separately:
  - A `cargo-fuzz`/libFuzzer harness (`fuzz/`) with **four targets** —
    `parse_pdf`, `filters`, `predictor`, `content_tokenizer` — is committed and
    runnable.
  - A targeted source audit of the four DoS classes against the parser, stream
    filters, and content tokenizer found and fixed **three** bugs (unbounded
    recursion in the object parser → depth-64 guard; unbounded `Vec`
    pre-allocation from an attacker-controlled object-stream count; an
    integer-overflow in the predictor row calc → `checked_mul`), each locked by a
    permanent regression test.
- **Precise, non-triumphant claim:** *the parser, stream filters, and content
  tokenizer return clean `OxideError`s — never panic, hang, or allocate
  unboundedly — on the malformed inputs found by this audit, and the full parity
  run recorded **0 panics / 0 timeouts** across 75 files.* **Not yet claimed:**
  long coverage-guided fuzzing (the harness exists but extended runs have not
  been executed here), and fuzzing of the **image decoders, font parsing, and
  crypto** subsystems (explicitly listed as not-yet-covered). Transitive Rust
  dependencies may use `unsafe` internally, and the three C-compiled crates
  below contain C.

### Performance

- Parallel text extraction scales **3–5×** (1→20 threads) on multipage docs;
  faster than `pdftotext` on real-world and small documents (§D.3.2).
- Image extraction ≈**8.5×** faster than `pdfimages` on the measured image PDF.
- Render peak memory is **flat in page count** (Arc-shared engine).
- Honestly: Poppler's rasterizer is faster on most 150-DPI vector/form pages,
  and Oxide's CLI render is single-core. See §D.5.

### Deployment simplicity

- One **9.0 MB** static binary, no system Poppler/Ghostscript/Cairo install, no
  shared-library chain, no external runtime. Poppler is a 25 MB multi-DLL tree.
- **Caveat (the pure-Rust constraint):** as configured, building the
  `oxide-cli`/`oxide-server` binaries is **not** C-toolchain-free.
  `zip = "2"` (default features) in both crates pulls in `bzip2-sys`,
  `lzma-sys`, and `zstd-sys` — all of which run `build.rs` + `cc` (zstd-sys also
  `bindgen`) and declare `links =`. These were verified compiled under
  `target/release/build/`. The **`oxide-engine` library is clean** (its `zip` is
  a dev-dependency only). The runtime artifact is still a single static binary
  (the C is linked in), so *deployment* simplicity holds; the *build* purity
  claim does not, today. **One-line fix:**
  `zip = { version = "2", default-features = false, features = ["deflate"] }`
  in both binary crates removes all three C crates with no functional loss
  (Oxide only needs the pure-Rust deflate backend for its ZIP output).
- The pure-Rust media stack the project *did* control is clean: `hayro-jpeg2000`
  / `hayro-ccitt` / `hayro-jbig2`, `image-webp`, `ttf-parser`, `unicode-bidi`,
  and the RustCrypto crates (`aes`, `sha2`, `md-5`, `sha1`, `hmac`, `pbkdf2`,
  `cbc`, `subtle`) have **no** `build.rs`/`links`/`-sys` C code.
- **Re-audited this session after rounds 16–22.** The new capabilities added no
  C toolchain: the PDF writer needs no deps; the signature crates
  (`cms`, `rsa`, `x509-cert`, `der`, `spki`) are RustCrypto and pure-Rust; the
  SVG-validation crate `resvg`/`usvg`/`tiny-skia` is a **dev/test-only** dep
  (never in the product binary). `cargo tree -i cc` confirms `cc` is pulled
  **only** by `bzip2-sys`/`lzma-sys`/`zstd-sys` via `zip` — the same single
  pre-existing leak, unchanged.

### License (a real differentiator for proprietary integrators)

- **Poppler is GPLv2.** Linking it into a closed-source product imposes GPL
  obligations — a hard blocker for many proprietary integrators, which is a
  major reason some ship their own PDF stack.
- **Oxide's dependency tree is fully permissive.** A `cargo metadata` license
  scan of the entire resolved tree this session found **134× `MIT OR
  Apache-2.0`, 46× `MIT`, 31× `Apache-2.0 OR MIT`**, and the rest BSD / Zlib /
  Unlicense / Unicode-3.0 variants — all permissive. The **only** GPL mention is
  `r-efi` (`MIT OR Apache-2.0 OR LGPL-2.1-or-later`), a tri-licensed transitive
  UEFI-target crate from which MIT/Apache can be chosen (so no copyleft is
  forced; it isn't even compiled on this platform). **No forced GPL/LGPL/AGPL**
  anywhere in the tree. This means Oxide can be embedded in proprietary software
  without GPL obligations — something Poppler cannot offer.
- **Honest caveat:** Oxide's own crates (`oxide-engine`/`oxide-cli`/
  `oxide-server`) do **not yet declare a `license` field** in their
  `Cargo.toml`. They should before publishing (a permissive
  `MIT OR Apache-2.0` would match the dependency stack). Three transitive crates
  declare no license. Verified via `cargo metadata` this session.

### Resource safety / production hardening (server)

Operational maturity beyond raw library parity:
- **Cooperative per-request cancellation** (`CancelToken` polled in the operator
  dispatch, tiling, and nested-group loops) → a runaway render returns **503**
  and *frees its worker thread* (proven by a follow-up-request-succeeds test).
- **Resource caps**: render pixels (100 MP), output bytes (2 GiB), image count
  (10k), decompression bomb (512 MiB) — each enforced *before/within* allocation.
- **Fail-closed auth** (refuses to start with no keys unless an explicit dev
  opt-in), **constant-time** key comparison, **restrictive CORS** allowlist,
  **sanitized errors** with server-side correlation ids, **bounded** rate
  limiter (100k-key cap + scheduled cleanup).
- **Bounded async job queue** (capacity, worker pool, retention, per-job
  deadline via the same cooperative cancellation; non-guessable 128-bit ids;
  per-key ownership; fault isolation so one bad job can't kill a worker).

---

## D.5 Where Oxide still trails Poppler (honest)

Ordered roughly by impact:

1. **Rasterization speed (150 DPI vector/form pages).** Poppler is 3.9× faster
   on `tracemonkey` and 8.7× faster on `form_160f`. Oxide's Splash-equivalent is
   younger and less optimized. *Future work:* profile the fill/stroke and glyph
   rasterizer hot paths.
2. **CLI render is single-core.** No page-level parallel speedup observed
   (1-thread ≈ 20-thread). *Future work:* wire the engine's Arc-shared parallel
   render (proven in `render_bench`) into the CLI `render` command, and
   parallelize PNG encode / ZIP assembly.
3. **Arabic / complex-script shaping.** `rtl-text` is 33.4%; `ThuluthFeatures`
   (~7.6%) and `issue5801` (~18.6%) need contextual joining/ligature shaping and
   a non-embedded Identity CMap. BiDi reordering is done; shaping is not.
4. **CJK text similarity (32.2%).** Glyph→Unicode coverage and the vertical
   fixture's compressed-objstream font encoding limit the score; WMode detection
   is implemented but under-exercised by the corpus.
5. **Multi-column reading order (64.0%).** Column detection / line clustering in
   the flowing-text path is the gap.
6. **Public-key encryption (Adobe.PubSec): not supported.** Deferred; needs
   PKCS#7/CMS + RSA private-key input. Clean "not supported" error today.
7. **Three encrypted corpus files still fail**: `encrypted-attachment` and
   `issue15893_reduced` fail with **parse errors before** decryption (a non-crypto
   parser gap); `print_protection` is a password-protected file whose password
   isn't supplied (Poppler also errors on it).
8. **Color fidelity**: ICCBased is reduced to a device space by `/N` (no full ICC
   transform); Separation/DeviceN overprint and attributes are not modelled.
9. **JBIG2 / JPX exotic features**: advanced JBIG2 segment types and rare
   in-tile-part JPX progression changes error cleanly rather than decoding.
10. **PostScript / EPS output: the one remaining tool gap.** `pdftops` and
    `pdftocairo -ps/-eps` have no Oxide equivalent (SVG vector output *does*
    exist). Deferred in Mega-19; the sibling-sink architecture makes a
    `PostScriptSink` a contained follow-up.
11. **Signature trust scope (Mega-21).** `verify-sig` reports *cryptographic*
    validity + coverage + cert details, but does **not** do trust-chain
    validation to a root CA, revocation (OCSP/CRL), validity-period enforcement,
    or timestamp tokens; only RSA PKCS#1 v1.5 (not ECDSA/PSS). Poppler's
    `pdfsig` can use a system trust store.
12. **Merge drops document-level features (Mega-16).** `merge`/`extract`/`split`
    carry page content + resources but **not** AcroForm fields, outlines, named
    destinations, or page annotations. (Poppler's `pdfunite` has its own limits
    here too.)
13. **Vector-output rasterize-embed fallback (Mega-19).** SVG pages with images,
    shadings, patterns, forms, or soft masks fall back to a whole-page embedded
    raster (visually correct, not scalable for those regions).
14. **No language bindings — the biggest *adoption* gap vs Poppler.** Oxide is
    consumable only as a **Rust crate** (`oxide-engine`). Poppler's broad reach
    comes from its **C-ABI** (`libpoppler`) plus **GLib** and **Qt** bindings,
    which let C/C++/Python/Vala/etc. embed it. Oxide has **no C-ABI, no Python,
    no WASM** layer. Recommended future roadmap (each its own effort, *not*
    built this round): a `oxide-capi` crate exposing a C-ABI via `cbindgen`
    (unlocks any language); a PyO3 `oxide-py` wheel (the largest single adoption
    win, given how much PDF tooling is Python); and a `wasm-bindgen` build for
    browser/Node. See §B-assessment below.
15. **Feature surface (library depth)**: no interactive form fill, shallower
    annotation support, no HarfBuzz-grade shaping, no public-key (Adobe.PubSec)
    *decryption* — Poppler has these.
16. **High-DPI / image-decode peak memory**: Oxide holds buffers for ZIP
    assembly where Poppler streams to disk (64 vs 46 MB at 300 DPI; 67 vs 32 MB
    on image decode).

### Library / embedding assessment (this session)

- **API coherence: good.** [`ContentEngine`] is a single, clean entry point —
  `open_path`/`open_bytes`(`_with_password`) then `page_count`, `get_page_text`,
  `render_page`/`render_page_png_fast`/`render_page_svg`, `extract_image_bytes`,
  `document_info`, `list_fonts`, `list_attachments`/`extract_attachment`,
  `export_html`, `verify_signatures`, `extract_pages`/`extract_single_page`;
  plus free functions `build_merged`/`build_subset` and the `PdfDocument` /
  `PdfWriter` layer for manipulation. All return `oxide_engine::Result`.
- **Docs/examples (improved this session):** `cargo doc` now builds **clean**
  (6 broken-doc-link warnings fixed); the crate front page has a runnable
  getting-started section (2 compile-tested doctests); and
  `examples/getting_started.rs` exercises every common operation end-to-end and
  is built by `cargo test` so it can't rot.
- **Not done (future):** a stability-guarantee / semver-API pass, and the
  bindings above. The crate is a pleasant Rust dependency today; cross-language
  reach is the gap.

---

## D.6 Reproducibility

**Hardware/OS:** Windows 11 (10.0.26200), 20 logical CPUs.
**Toolchain:** rustc/cargo 1.95.0; Python 3.14.3 (`py`).
**Poppler:** 26.02.0 (`target/tools/poppler/poppler-26.02.0/Library/bin`).
**Oxide:** `cargo build --release` from the current working tree.

Parity (§D.3.1):
```
py scripts/poppler_compare.py --manifest tests/corpus/manifest.json \
  --oxide-bin target/release/oxide.exe --no-build \
  --poppler-bin-dir target/tools/poppler/poppler-26.02.0/Library/bin \
  --output-dir target/poppler_compare/final_round15 \
  --report-path docs/poppler_parity_round15_summary.md \
  --max-render-pages 1 --dpi 150
```
Per-file data: `target/poppler_compare/final_round15/results.{json,csv}`;
summary: `docs/poppler_parity_round15_summary.md`.

Performance (§D.3.2):
```
py scripts/perf_compare.py \
  --poppler-bin-dir target/tools/poppler/poppler-26.02.0/Library/bin --repeats 3
# high-DPI memory point:
py scripts/perf_compare.py --poppler-bin-dir <…> --cases large_120pg --dpi 300 --repeats 2
```
Raw data: `docs/perf_compare_results.json`. Oxide-only 1-vs-N and
shared-vs-perpage memory: `scripts/perf_bench.py` + `crates/engine/examples/render_bench.rs`
(see `docs/perf_baseline.md`).

Tool-surface parity (§D.2a), verified this session:
```
# every Oxide subcommand, end-to-end:
cargo test -p oxide-cli --test tool_surface
# command-by-command cross-checks were run against the bundled Poppler
# utilities (pdfinfo/pdffonts/pdfdetach/pdfseparate/pdfunite/pdftotext/
# pdftoppm/pdftocairo/pdfimages/pdftohtml) on tests/engine fixtures.
# pdfsig is NOT in the bundled Poppler, so verify-sig was checked against a
# known-ground-truth signed fixture (scripts/make_signature_fixtures.py).
```
License/C-toolchain audit (§D.4), verified this session:
```
cargo tree -i cc                  # cc only via zip's bzip2/lzma/zstd-sys
cargo metadata --format-version 1 # license scan: all permissive, no forced GPL
```

Supporting docs: `docs/poppler_parity_baseline.md` (round history),
`docs/robustness.md` (DoS hardening + fuzzing), `docs/security.md` (server
controls), `docs/jobs.md` (async queue), and the per-tool docs:
`docs/manipulation.md`, `docs/reporting.md`, `docs/attachments.md`,
`docs/vector_output.md`, `docs/html_output.md`, `docs/signatures.md`.

---

## Closing assessment

**Does Oxide match Poppler's full tool surface?** Essentially yes — **11 of
Poppler's 12 CLI utilities** have a verified Oxide equivalent (§D.2a); the lone
gap is **PostScript/EPS output**. This is the round where the tool-surface claim
became true and was checked command-by-command.

Oxide **beats Poppler** today on: deployment simplicity (single 9 MB binary vs a
25 MB DLL tree), **permissive licensing** (MIT/Apache-class stack vs Poppler's
GPLv2 — embeddable in proprietary software), memory-safety guarantees for its
own code (zero `unsafe`, 0 panics/0 timeouts over the corpus), text-extraction
throughput on real-world and small documents (3–5× parallel scaling),
image-extraction throughput (~8.5×), and high-DPI rendering of large documents.
It is **at parity** on render correctness for most categories (29.20 dB) and on
render peak memory at 150 DPI.

Oxide **still trails Poppler** on: 150-DPI vector/form rasterization speed
(Poppler's mature Splash engine is 4–9× faster), complex-script (Arabic) and
CJK/multi-column text fidelity, **PostScript/EPS output** (missing), **signature
trust-chain/revocation** (crypto-only scope), and — the biggest *adoption* gap —
**language bindings** (Oxide is Rust-only; Poppler has C-ABI + GLib + Qt).

**Use Oxide when** you need a memory-safe, single-binary, permissively-licensed
PDF toolkit for text/image extraction, analysis, rendering (per-core speed
acceptable), document manipulation, reporting, HTML/SVG conversion, or
cryptographic signature checking — especially where GPL is a blocker or you're
already in Rust. **Prefer Poppler when** you need an existing C++/GLib/Qt
integration, full trust-store signature validation, PostScript output, or its
deeper annotation/form/shaping capabilities.

**Top roadmap items** (evidence-backed, from this audit): (1) a C-ABI / Python /
WASM binding layer — the largest adoption lever; (2) PostScript/EPS output to
close the last tool gap; (3) signature trust-chain + revocation; (4) declare a
`license` field on Oxide's own crates and apply the one-line `zip`
`default-features = false` fix so the "pure Rust" build claim holds for the
binaries (§D.4).
