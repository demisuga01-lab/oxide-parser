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

## What was set up

A `cargo-fuzz` / libFuzzer harness lives in the out-of-tree [`fuzz/`](../fuzz)
workspace member (excluded from the stable workspace so `cargo build`/`cargo
test` stay green without nightly). Six targets:

| Target              | Entry point                          | Subsystem |
|---------------------|--------------------------------------|-----------|
| `parse_pdf`         | `ContentEngine::open_bytes(bytes)`   | Document parser: header, xref/trailer, object streams, recursive object parser |
| `filters`           | `filters::fuzz_decode_filter`        | Flate / LZW / ASCIIHex / ASCII85 / RunLength decoders |
| `predictor`         | `filters::fuzz_apply_predictor`      | PNG/TIFF predictor stage |
| `content_tokenizer` | `ContentParser::parse(bytes)`        | Content tokenizer + inline-image state machine + operand parser |
| `image_decoders`    | `fuzz::fuzz_decode_image`            | CCITT G3/G4, JBIG2, JPEG2000 (JPX), DCT/JPEG decode paths (selector byte picks the codec) |
| `fonts`             | `fuzz::fuzz_parse_font`              | Font-program parsing (TrueType / CFF / OpenType / bare-CFF) via the glyph-outline extractor |

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

The parser, stream filters, content tokenizer, image decoders, and font parser
return cleanly — never panic, hang, or allocate unboundedly — on the malformed
inputs exercised. **Four** distinct DoS bugs (one unbounded recursion, one
unbounded allocation, one integer-overflow, and one inline-image infinite loop)
were found and fixed, each guarded by a permanent regression test.

## Fuzzing runs actually executed (this round)

All six targets were built and run on nightly + cargo-fuzz 0.13.1
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

The single finding (the inline-image infinite loop) was minimized, fixed, and
turned into a regression test; its minimized input is kept as a corpus seed.

## Honest limitations

- **These are short, bounded runs** (≈1–3 min per target), enough to confirm
  the harness works on this platform, to re-exercise the seeded corpora, and to
  surface one real hang — but **not** the multi-hour coverage-guided campaigns
  that find deep bugs. The claim is "fuzzed clean for the durations above, with
  one found bug fixed," not "fuzzed clean for N hours."
- **Coverage now includes** the parser, stream filters, predictor, content
  tokenizer, **image decoders** (CCITT/JBIG2/JPX/DCT), and **font parsing**
  (TrueType/CFF/bare-CFF) — the image and font subsystems flagged as gaps in the
  prior round are now fuzzed.
- **Still not fuzzed**: the **crypto** path (`crypto.rs` — malformed encryption
  dictionaries / password handling) and the higher-level render pipeline. The
  image/font decoders largely wrap third-party crates (`hayro-*`, `ttf-parser`,
  `jpeg-decoder`); a crash inside a dependency would be an upstream bug, but
  Oxide's wrappers are expected to guard sizes/return errors regardless — no
  such dep-level crash surfaced in these runs.
- `cargo-fuzz` requires nightly Rust; the `fuzz/` crate is intentionally
  outside the stable workspace, so the stable `cargo build`/`cargo test` never
  sees it.

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

