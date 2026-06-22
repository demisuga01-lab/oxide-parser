# Robustness & memory-safety posture

> For the server's **security** controls — fail-closed API-key auth, restrictive
> CORS, sanitized error responses, and bounded rate-limiter memory — see
> [`security.md`](security.md). This document covers resource-safety and
> memory-safety (DoS-class) hardening.

Oxide is pure safe Rust. By construction it cannot exhibit the buffer
overflows, use-after-free, and type-confusion bugs that have produced a long
history of CVEs in C/C++ PDF stacks such as Poppler: safe Rust has no raw
pointer arithmetic and enforces bounds and lifetimes at compile time.

That eliminates *memory-corruption* bugs. It does **not** automatically
eliminate the denial-of-service class that survives in safe code:

- **Panics** — out-of-bounds indexing, `unwrap()`/`expect()` on attacker-
  controllable `None`/`Err`, `unreachable!()`, integer-overflow panics under
  overflow checks. In the server every panic is a request-killing DoS.
- **Hangs / infinite loops** — loops driven by attacker-controlled data
  without a bound (xref `/Prev` chains, decode loops, object resolution).
- **Unbounded allocation** — a size/count field used to reserve memory before
  validating it against the actual input size, turning a tiny file into a
  multi-gigabyte allocation and an OOM abort.
- **Stack overflow** — unbounded recursion on deeply-nested structures.

This document records what has been hardened against those classes in the
parser, the stream filters, and the content-stream tokenizer.

GA Prompt 5 extends this posture to the whole SDK surface introduced by the
enterprise prompts: modern writer modes, linearization, PDF/A conversion,
editing/redaction/forms, and signature validation. See
[`ga5_release_hardening.md`](ga5_release_hardening.md) for the new fuzz targets
and 265-file cross-pillar corpus run.

Continuous private-CI fuzzing is configured in `.github/workflows/fuzz.yml`.
See [`continuous_fuzzing.md`](continuous_fuzzing.md) for the persistent corpus,
regression gate, and crash triage workflow.

Differential fuzzing is configured in
`.github/workflows/differential-fuzz.yml`. See
[`differential_fuzzing.md`](differential_fuzzing.md) for the qpdf/Poppler
wrong-output gate.

## What was set up

A `cargo-fuzz` / libFuzzer harness lives in the out-of-tree [`fuzz/`](../fuzz)
workspace member (excluded from the stable workspace so `cargo build`/`cargo
test` stay green without nightly). Ten targets were built and run in the Prompt
G safety pass:

| Target              | Entry point                          | Subsystem |
|---------------------|--------------------------------------|-----------|
| `parse_pdf`         | `ContentEngine::open_bytes(bytes)`   | Document parser: header, xref/trailer, object streams, recursive object parser |
| `filters`           | `filters::fuzz_decode_filter`        | Flate / LZW / ASCIIHex / ASCII85 / RunLength decoders |
| `predictor`         | `filters::fuzz_apply_predictor`      | PNG/TIFF predictor stage |
| `content_tokenizer` | `ContentParser::parse(bytes)`        | Content tokenizer + inline-image state machine + operand parser |
| `image_decoders`    | `fuzz::fuzz_decode_image`            | CCITT G3/G4, JBIG2, JPEG2000 (JPX), DCT/JPEG decode paths (selector byte picks the codec) |
| `fonts`             | `fuzz::fuzz_parse_font`              | Font-program parsing (TrueType / CFF / OpenType / bare-CFF) via the glyph-outline extractor |
| `cmap`              | `fuzz::fuzz_parse_cmap`              | ToUnicode CMap parsing and lookup |
| `crypto`            | `fuzz::fuzz_crypto`                  | Standard encryption dictionary parsing, password verification, key derivation, stream decrypt |
| `functions`         | `fuzz::fuzz_functions`               | PDF Function Types 0/2/3/4, sampled-function bit reader, Type 4 PostScript calculator |
| `writer`            | `fuzz::fuzz_writer`                  | Object serialization, string/name/stream escaping, tiny output-PDF generation |

Seed corpora (valid objects, content streams, encoded filter data, bundled
font programs) are under `fuzz/corpus/<target>/`. See
[`fuzz/README.md`](../fuzz/README.md) for how to run, minimise, and replay.

## Findings and fixes

Each subsystem was audited against the four DoS classes above, and each
confirmed bug was fixed to return a clean `OxideError` (never crash or hang)
and locked down with a permanent regression test feeding the malicious input
to the function and asserting `Err`.

| # | Class | Location | Root cause | Fix | Regression test |
|---|-------|----------|------------|-----|-----------------|
| 1 | Stack overflow (unbounded recursion) | `parser.rs` `parse_object`→`parse_array`/`parse_dictionary` | Recursive-descent object parser had no nesting bound; input like `[[[[…` thousands deep overflows the stack and aborts the process | Added `MAX_PARSE_DEPTH = 64` with enter/leave guards on every nested structure; over-deep input returns `ParseError` | `parser::tests::deeply_nested_arrays_error_instead_of_overflowing_stack`, `…_dictionaries_…`, `nesting_within_limit_still_parses_and_depth_resets` |
| 2 | Unbounded allocation (OOM) | `reader.rs` `parse_object_stream_data` | `Vec::with_capacity(n)` where `n` is the attacker-controlled `/N` object-stream count; a tiny stream declaring `/N 4000000000` reserves ~tens of GB before any per-entry validation | Capacity hint bounded by `n.min(first)` (a real entry needs ≥1 header byte, so the count cannot exceed the header length); the read loop still errors cleanly on a truncated header | `reader::tests::object_stream_huge_n_does_not_allocate_or_panic` |
| 3 | Integer-overflow panic / size-field misvalidation | `filters.rs` `apply_predictor` | `columns * colors * bits_per_component` (all attacker-controlled via DecodeParms) multiplied without overflow check — panics under overflow checks, silently wraps to a bogus row length otherwise | Switched to `checked_mul`; overflow returns `MalformedPdf` | `filters::tests::predictor_row_dimensions_overflow_returns_err_not_panic` |
| 4 | **Infinite loop (hang / CPU-DoS)** — *found by libFuzzer (`content_tokenizer`) this round* | `content/tokenizer.rs` `read_inline_image_data` + `content/parser.rs` `parse_tokens` | An inline image (`BI`/`ID`) with no `EI` terminator made `read_inline_image_data` return a token error **without advancing `pos` or leaving the `Data` state**; `ContentParser::parse` recovers from token errors with `continue`, so it called the tokenizer again at the same position, got the same error, and looped forever (100% CPU, no termination). | On no-`EI`, consume the remaining bytes as the (unterminated) inline-image payload, advance to EOF, and leave the inline-image state so iteration terminates. The previously-hanging libFuzzer input now executes in ~1 ms. | `content::tokenizer::tests::unterminated_inline_image_terminates_and_does_not_hang` (+ the minimized input saved as a corpus seed) |

| 5 | Panic (malformed encryption dictionary) | `crypto.rs` `EncryptionInfo::from_dict` / `compute_encryption_key` | The new `crypto` fuzz target generated a legacy V1-V4 encryption dictionary with `/Length 256`. The parser accepted it, then key derivation tried to slice 32 bytes from a 16-byte MD5 digest. | Reject legacy `/Length` values outside 40..128 bits in 8-bit increments; `compute_encryption_key` also clamps defensively so direct internal calls cannot panic. | `crypto::tests::from_dict_rejects_invalid_legacy_key_length`, `crypto::tests::compute_encryption_key_defensively_caps_invalid_legacy_length` |
| 6 | Resource cap too loose for a 1 GB render harness | `engine.rs` `DEFAULT_MAX_RENDER_PIXELS` | A real pdf.js image fixture (`issue19517.pdf`) declares a 12608x16806 pt page: 211,890,048 pixels at 72 DPI. The old 300 MP engine default admitted it, so the outer 1 GB harness memory cap killed both Oxide and Poppler. | Lowered the default engine/CLI render cap to 100 MP, matching the server default. The page now fails in 56 ms with a clean `ResourceLimit` and 3.1 MB peak RSS. | `render_resource_limits::oversized_real_world_page_is_capped_at_default_limit` |

### Audited and already robust (no change needed)

- **Stream decoders** (`ASCII85`, `RunLength`, `LZW`, `ASCIIHex`): all use
  `checked_add` for run/offset arithmetic, bounds-check every slice, and never
  preallocate from an attacker-controlled size field. LZW caps its table at
  4096 entries.
- **xref-stream entry parser** (`parse_xref_stream_entries`): no preallocation
  (`Vec::new()`), bounds-checks `end > raw.len()` before each entry read, so a
  huge `/Index` count errors quickly instead of allocating.
- **Content tokenizer & parser**: fully iterative — the operand/array stack
  uses a `saturating_add` depth counter and the inline-image state machine
  bounds-checks every slice. No recursion, no size-field preallocation. (One
  termination bug in the inline-image path — finding #4 above — was found by
  fuzzing this round and fixed.)
- **xref `/Prev` chain following**: already guarded by a `visited` set (cycle
  detection), so a `/Prev` loop terminates.
- **Reference resolution** and **PostScript calculator functions**: depth-64
  limits; **Form XObject** nesting: depth-8 limit (pre-existing).

## Current posture

The parser, stream filters, content tokenizer, image decoders, font parser,
CMap parser, crypto primitives, function evaluator, and writer
return cleanly — never panic, hang, or allocate unboundedly — on the malformed
inputs exercised. **Six** distinct robustness bugs or resource gaps have been
found and fixed so far: unbounded recursion, unbounded allocation, integer
overflow, an inline-image infinite loop, invalid legacy encryption key length,
and an over-loose default render pixel cap. Each is guarded by a permanent
regression test.

## Fuzzing runs actually executed (this round)

All ten targets were built and run on nightly + cargo-fuzz 0.13.1
(x86_64-pc-windows-msvc), release with debug-assertions + overflow-checks on,
seeded from the committed corpora:

| Target | Executions | Wall time | Result |
|---|---:|---:|---|
| `parse_pdf` | 3,023,672 | 61 s | clean |
| `filters` | 924,860 | 181 s | clean |
| `predictor` | 1,801,592 | 121 s | clean |
| `content_tokenizer` | (pre-fix) hung on 1 input → fixed → 740,817 | 121 s | **1 bug found+fixed** (finding #4), clean after fix |
| `image_decoders` | 1,187,273 | 181 s | clean |
| `fonts` | 503,991 | 181 s | clean |
| `cmap` | 5,459 | 31 s | clean after adding a 65,536 mapping cap |
| `crypto` | 26,473 | 61 s | **1 bug found+fixed** (finding #5), clean after fix |
| `functions` | 182,612 | 61 s | clean |
| `writer` | 384,711 | 61 s | clean |

The new finding in this pass was the crypto `/Length` panic. The CMap run also
motivated a hard per-CMap mapping cap; it did not crash, but without the cap a
tiny malicious CMap could drive unnecessary map growth. Both changes are now
covered by regression tests.

## Honest limitations

- **These are short, bounded runs** (≈1–3 min per target), enough to confirm
  the harness works on this platform, to re-exercise the seeded corpora, and to
  surface real bugs — but **not** the multi-hour coverage-guided campaigns
  that find deep bugs. The claim is "fuzzed clean for the durations above, with
  the findings listed here fixed," not "fuzzed clean for N hours."
- **Coverage now includes** the parser, stream filters, predictor, content
  tokenizer, image decoders (CCITT/JBIG2/JPX/DCT), font parsing,
  ToUnicode CMaps, crypto dictionary/key/decrypt paths, PDF Functions, and writer serialization.
- **Still not fuzzed directly**: the full raster render pipeline and the C API
  FFI boundary. Rendering is covered by the isolated subprocess benchmark
  harness; the C API is covered by unit tests, but a cargo-fuzz ASan target could
  not be linked on MSVC because the crate also builds as `cdylib`.
- The image/font/shaping paths wrap Rust dependency crates (`hayro-*`,
  `ttf-parser`, `jpeg-decoder`, `rustybuzz`). A crash inside a dependency would
  be an upstream bug, but Oxide's wrappers are expected to validate sizes and
  return errors regardless; no dependency-level crash surfaced in these runs.
- `cargo-fuzz` requires nightly Rust; the `fuzz/` crate is intentionally
  outside the stable workspace, so the stable `cargo build`/`cargo test` never
  sees it.

## Prompt G CVE-class corpus run

The Prompt G safety corpus is saved at
[`renderer-benchmark/corpus/prompt-g-cve-class-manifest.json`](../renderer-benchmark/corpus/prompt-g-cve-class-manifest.json).
It contains 751 files selected from the expanded pdf.js corpus plus generated
hostile fixtures to mirror CVE-class input shapes: malformed/truncated streams,
bad xrefs/object streams/startxref, resource bombs, malformed image codecs,
malformed fonts/CMaps, malformed encryption/signature dictionaries, and inert
active-content fixtures.

Class counts from the manifest:

| Class | Files |
|---|---:|
| Public suite fuzzed/bug-regression fixtures | 622 |
| Hostile generated fixtures | 60 |
| Image-codec fixtures | 28 |
| Resource-bound fixtures | 27 |
| Font/CMap fixtures | 23 |
| Active-content fixtures | 18 |
| Xref/object-stream/structure fixtures | 13 |
| Crypto/signature fixtures | 7 |

Run command:

```powershell
py renderer-benchmark\scripts\renderer_benchmark.py `
  --manifest renderer-benchmark\corpus\prompt-g-cve-class-manifest.json `
  --oxide-bin target\release\oxide.exe `
  --poppler-bin-dir target\tools\poppler\poppler-26.02.0\Library\bin `
  --output-dir renderer-benchmark\results\prompt-g-cve-class `
  --dpi 72 --max-pages-per-file 1 --timeout-sec 15 --max-memory-mb 1024 `
  --determinism-sample 0
```

Full run result:

- Files processed: 751.
- Hostile subset: 60/60 crash-free, timeout-safe, and memory-bounded.
- All-file Oxide crashes/panics: 0.
- Active-content fixtures: rendered as inert PDF content; Oxide has no
  JavaScript/Launch execution path.
- Initial all-file resource findings: 2.
  - `pdfjs_full_issue19517`: both Oxide and Poppler crossed the 1 GB harness
    memory cap on a 211.9 MP page. Fixed by lowering Oxide's default render cap
    to 100 MP; targeted rerun returns a clean `ResourceLimit` in 56 ms with
    3.08 MB peak RSS.
  - `pdfjs_full_issue840`: timed out under the 15 s safety harness but completed
    cleanly under a 30 s targeted rerun (12.4 s, 18.4 MB peak RSS). This is a
    performance/visual-fidelity item, not a crash/hang/OOM.

After the targeted fixes/reruns, the safety statement for the corpus is:
0 Oxide panics/aborts, no unbounded memory growth, hostile fixtures bounded by
the harness, and every discovered malformed-input panic/resource issue fixed or
converted into a clean error.

## AddressSanitizer and `unsafe`

The core engine, CLI, and server have no workspace-owned `unsafe` blocks. The
`oxide-capi` crate necessarily contains FFI `unsafe` around raw pointers and
ownership transfer. `cargo test -p oxide-capi` passed, and an ASan libFuzzer
replay of `parse_pdf` passed, but a direct `oxide-capi` fuzz target could not be
linked on Windows/MSVC because the crate also emits `cdylib` and the fuzz link
failed with an unresolved `main` symbol. Treat the C API FFI boundary as audited
and unit-tested, but not yet ASan-fuzzed.

## Poppler comparison framing

Oxide's factual differentiator is class elimination, not perfection: safe Rust
eliminates buffer-overflow, use-after-free, and out-of-bounds-write bugs in the
core engine/CLI/server by construction, while Poppler is a C++ stack that must
continue patching memory-safety and malformed-document crash classes. Public
evidence used for this framing:

- Poppler's 26.02.0 release notes include crash fixes for malformed documents:
  <https://poppler.freedesktop.org/releases.html>
- Public advisories document Poppler memory-safety vulnerabilities as a real
  class, for example GitHub Security Lab's CVE-2025-52886 use-after-free
  advisory:
  <https://securitylab.github.com/advisories/GHSL-2025-054_poppler/>

## Resource safety: per-request timeout and limits (server)

The fuzzing round above hardened the **parser** against *malformed* input at
the **parse** level. This section covers the complementary threat: inputs that
are **well-formed enough to parse** but are deliberately abusive in *scale* or
*structure*, designed to exhaust CPU, memory, or time at the **processing**
(render/extract) level. These defenses live in the server's request path
(`crates/server/src/processing.rs`) plus cooperative cancellation hooks in the
engine.

### Per-request processing timeout (cooperative cancellation)

A single crafted page — a content stream with millions of operators, a
pathological tiling pattern, deeply nested Form XObjects — can occupy a worker
thread far longer than any legitimate request. The heavy work runs CPU-bound
inside `tokio::task::spawn_blocking` + rayon.

A `tower::TimeoutLayer` is **insufficient** here: it times out the *async
future* and returns an error to the client, but the blocking thread keeps
running, still pegging a CPU core. Under attack that leaks workers — the exact
DoS we're defending against. Rust has no safe thread cancellation, so the only
correct fix is **cooperative cancellation**:

1. The server creates a [`CancelToken`](../crates/engine/src/cancel.rs) (a
   shared `Arc<AtomicBool>`) per request and arms a timer task that trips it at
   `OXIDE_REQUEST_TIMEOUT_SECS`.
2. The token is threaded into `render_page_cancellable`. The engine's hot loops
   poll it and bail out early when set:
   - the **operator dispatch loop** (`dispatch_all`) checks every 64 operators;
   - the **tiling-pattern tile loop** checks once per tile;
   - child render states (Form groups, soft masks) **share the same token**, so
     nested work stops too.
3. Because all rayon page-workers share the one token, when the deadline hits
   they **all** observe it and bail, freeing every thread promptly.
4. The early exit becomes `OxideError::Cancelled`, which the server maps to a
   clean **503 Service Unavailable** ("request exceeded the processing time
   limit") — never a 500 or a hang.
5. The timer is aborted the instant the work finishes, so no timer leaks on the
   normal (fast) path.

The polling is a relaxed atomic load amortised over many operators, so normal
rendering throughput is unaffected.

A coarse async backstop (`with_deadline`, timeout + 5s grace) wraps the whole
handler so even a non-engine stall can't hang a request forever; the inner
cooperative timeout wins the race in practice so clients get the specific
message.

### Resource limits (memory / output / work)

Beyond the existing `OXIDE_MAX_FILE_SIZE` / `OXIDE_MAX_DPI` / `OXIDE_MAX_PAGES`
caps, three processing-level limits close resource-exhaustion vectors that
well-formed input can still hit:

| Limit | Env var (default) | Vector | Enforcement point |
|-------|-------------------|--------|-------------------|
| Render pixels per page | `OXIDE_MAX_RENDER_PIXELS` (100 MP) | "Pixel explosion": a giant MediaBox at a legal DPI demands billions of pixels → gigabytes of buffer → OOM | **Before** buffer allocation, from the page viewport (`check_render_pixels`). Hard-reject with 413 — clamping a true bomb is unsafe. |
| Total output bytes | `OXIDE_MAX_OUTPUT_BYTES` (2 GiB) | "Output explosion": a small input producing a huge ZIP (many large images / pages) | **During** output accumulation (`check_output_size`), so the oversized payload is never fully buffered. 413. |
| Image count | `OXIDE_MAX_IMAGE_COUNT` (10 000) | extract-images on a PDF with an absurd number of images | **Before** decode/encode of any image. 413. |
| Decompressed stream bytes | engine const `MAX_FLATE_DECOMPRESSED_BYTES` (512 MiB) | "Decompression bomb": a tiny FlateDecode stream inflating to gigabytes | **Inside** the flate decode loop via `Read::take` — stops inflating past the cap rather than checking after. `MalformedPdf`. |

Defaults are chosen to comfortably admit real workloads (an A4 page at 600 DPI
is ~35 MP; a 200-page render ZIP stays well under 2 GiB) while rejecting absurd
ones. The pixel cap keys on **actual pixels** (MediaBox × DPI²), so the same
giant page renders fine at a low DPI and is only rejected when it would truly
explode.

The decompression-bomb cap is an **engine-level** hard floor (in `filters.rs`),
so the CLI, tests, and any embedder are protected even without the server; the
server layers its tighter, configurable per-request caps on top.

### Pathological-input test coverage

Valid-but-abusive fixtures are generated programmatically (self-documenting,
reproducible) in
[`crates/server/tests/pathological/mod.rs`](../crates/server/tests/pathological/mod.rs)
and exercised by
[`crates/server/tests/pathological_hardening.rs`](../crates/server/tests/pathological_hardening.rs):

- **Slow render** (many full-page fills): returns 503 within a bounded time,
  and — critically — a **follow-up normal request still succeeds**, proving the
  worker threads were actually *freed* (cooperative cancellation worked), not
  leaked.
- **Giant MediaBox**: rejected with 413 before allocation at a normal DPI;
  renders fine at a tiny DPI (cap keys on pixels, not page size).
- **Deeply nested Form XObjects** (64 deep) and **self-referential Form**
  (A→A cycle): the depth-8 guard / cycle detection holds — the page still
  renders, no stack overflow, no infinite recursion.
- **Pathological tiling pattern** (tiny step, huge fill): terminates promptly
  via the 20 000-tile cap and/or timeout.
- **Decompression bomb**: rejected with bounded memory (engine unit test).
- **Too many pages**: rejected by the page cap with a clean 4xx.

### Async job queue (bounded background processing)

The heavy endpoints have async job variants (`/api/v1/jobs/...`, see
[`docs/jobs.md`](jobs.md)). Beyond moving long renders off the request
connection, the job system **adds resource-safety properties** of its own — it
is bounded in every dimension, consistent with the per-request model above:

- **Bounded queue** (`OXIDE_JOB_QUEUE_CAPACITY`): submissions past the cap are
  rejected with **503 + `Retry-After`** rather than accepted into unbounded
  backlog. Bursts are absorbed up to the cap, then shed cleanly.
- **Bounded worker pool** (`OXIDE_JOB_WORKERS`): a fixed number of background
  workers process at controlled concurrency — inbound submissions never each
  consume a processing slot the way sync requests do.
- **Bounded retained state** (`OXIDE_MAX_JOBS` + `OXIDE_JOB_RETENTION_SECS`): a
  cleanup task (same scheduling pattern as the rate-limiter reaper) drops jobs
  past their retention window and deletes their on-disk result files, so neither
  the in-memory store nor the result temp-dir grows without limit.
- **Per-job deadline** (`OXIDE_JOB_TIMEOUT_SECS`): larger than the sync cap
  (that is the point of async) but still bounded, and enforced via the **same
  cooperative cancellation** — on expiry the job is marked `failed` and its
  worker thread is freed.
- **Fault isolation**: a single job's error or panic is caught, marks that job
  `failed`, and the worker continues — one bad input can never take down a
  worker or the server. Per-job errors are classified/sanitized like the sync
  path (no internal leakage).

This keeps the in-memory/single-process scope (state lost on restart, no
horizontal scaling) as a documented boundary; the `JobStore` trait is the seam
for a future persistent backend.

### Future work

- **Process-level isolation** would give ultimate containment (a runaway or
  OOMing job killed with the process) at the cost of IPC/serialization
  complexity. Cooperative cancellation is the right tradeoff for this round;
  process isolation is the next escalation if untrusted multi-tenant isolation
  is ever required.
- The **extract-images decode path** is bounded by the image-count, output-size
  and decompression caps and the async backstop, but does not yet thread the
  `CancelToken` into per-image decode the way rendering does — a candidate for
  the next round.
