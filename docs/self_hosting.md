# Self-Hosting Oxide

Run the whole document-extraction stack on **your own machine or VPS** — CLI,
library, and HTTP API — with documents never leaving your hardware and no
per-page cloud fees. This guide covers the single-binary CLI, the self-hostable
server (with and without OCR), example workflows, and resource/privacy guidance.

> **TL;DR.** `cargo build --release` gives you a single `oxide` binary (CLI) and
> an `oxide-server` binary (HTTP API). The CLI needs no configuration. The
> server is **fail-closed**: it refuses to start until you set an API key (or
> explicitly opt into unauthenticated dev mode). OCR is an opt-in build feature
> that shells out to an external `tesseract` binary.

---

## 1. The single-binary CLI

### Build / install

```sh
# From a clone of the repo:
cargo build --release -p oxide-cli      # produces target/release/oxide
# Optionally with OCR (see §4):
cargo build --release -p oxide-cli --features ocr
```

The result is one static-ish binary with no Python, no Poppler/Ghostscript, no
ML runtime. Drop it anywhere on your `PATH`.

Check what you built — `--version` reports the engine version and whether OCR
was compiled in:

```text
$ oxide --version
oxide 0.1.0
engine: 0.1.0
ocr: not compiled-in (rebuild with --features ocr to enable)
features: []
```

### Command groups

```sh
# PARSE / EXTRACT (the document parser)
oxide parse          input.pdf --format markdown|json|html   # canonical model
oxide chunk          input.pdf --target-tokens 512 --overlap 64   # RAG chunks (JSON)
oxide extract-fields input.pdf --type auto|invoice|receipt|form|generic   # key-value (JSON)
oxide extract-text   input.pdf [--structured|--semantic|--ocr]   # plain / layout / OCR text
oxide extract-tables input.pdf --format csv|json|html

# STRUCTURAL (qpdf-class; read-and-rewrite)
oxide merge a.pdf b.pdf -o merged.pdf
oxide split input.pdf -o "page-%d.pdf"
oxide extract-pages input.pdf 1,3,5-9 -o subset.pdf

# INSPECT
oxide info   input.pdf [--json]      # pdfinfo-style + parser facts
oxide fonts  input.pdf [--json]
oxide detach input.pdf --list        # attachments

# RENDER (feeds OCR + previews)
oxide render input.pdf --dpi 150 --format png
```

`--ocr`, `--ocr-lang`, and `--ocr-dpi` engage OCR on `parse`, `extract-fields`,
`chunk`, and `extract-text` when the binary was built `--features ocr` (see §4).
A binary built without OCR returns an actionable error if you pass `--ocr`.

> **Structural writes.** The CLI includes `encrypt`, `rotate`, `optimize`,
> `repair`, and a guarded qpdf-validated `linearize` subset. Broader
> linearization coverage, decrypt-as-write, and server mutation routes remain
> deliberate follow-ups (see `docs/manipulation.md`).
> `extract-tables` does not support `--ocr` (OCR'd table-grid reconstruction is
> a known gap; use `extract-fields --ocr` for scanned tabular data).

---

## 2. Running the server locally

The server (`oxide-server`) is an HTTP API over the same engine: parse, chunk,
extract-fields, info, extract-text, analyze, render (pdf2img), and extract-images,
with auth, rate limiting, resource caps, and an async job queue for large inputs.

### Quick start (development)

The server is **fail-closed**: with no API keys configured it refuses to start,
to prevent a forgotten config from silently exposing every endpoint. For local
development you can either set a key or explicitly opt into unauthenticated mode:

```sh
# Option A — set an API key (recommended, mirrors production):
OXIDE_API_KEYS=dev-secret-key cargo run --release -p oxide-server

# Option B — explicit dev escape hatch (NEVER in production):
OXIDE_ALLOW_UNAUTHENTICATED=true cargo run --release -p oxide-server
```

Then call it (authenticated example):

```sh
# Parse a PDF to Markdown:
curl -sS -H "X-API-Key: dev-secret-key" \
  -F "file=@input.pdf" -F "format=markdown" \
  http://localhost:8080/api/v1/parse

# RAG chunks (JSON):
curl -sS -H "X-API-Key: dev-secret-key" \
  -F "file=@input.pdf" -F "target_tokens=512" \
  http://localhost:8080/api/v1/chunk

# Key-value fields (JSON):
curl -sS -H "X-API-Key: dev-secret-key" \
  -F "file=@invoice.pdf" -F "doc_type=invoice" \
  http://localhost:8080/api/v1/extract-fields

# Document metadata (JSON):
curl -sS -H "X-API-Key: dev-secret-key" -F "file=@input.pdf" \
  http://localhost:8080/api/v1/info
```

Health and readiness probes are auth-exempt:

```sh
curl -sS http://localhost:8080/health      # -> ok
curl -sS http://localhost:8080/readiness   # -> {"status":"ready",...}
```

### Endpoints

| Method | Path | Purpose |
| --- | --- | --- |
| POST | `/api/v1/parse` | Canonical model → Markdown / JSON / HTML |
| POST | `/api/v1/chunk` | RAG-ready semantic chunks (JSON) |
| POST | `/api/v1/extract-fields` | Key-value fields (JSON) |
| POST | `/api/v1/info` | Document metadata (JSON) |
| POST | `/api/v1/extract-text` | Plain / structured text |
| POST | `/api/v1/analyze` | Text-layer / scanned detection |
| POST | `/api/v1/pdf2img` | Render pages to a ZIP of images |
| POST | `/api/v1/extract-images` | Extract embedded images (ZIP) |
| POST | `/api/v1/jobs/pdf2img`, `/api/v1/jobs/extract-images` | Async variants for large inputs |
| GET | `/api/v1/jobs/{id}`, `/api/v1/jobs/{id}/result` | Poll / download job result |
| GET | `/api/v1/version`, `/health`, `/readiness` | Versions / probes |

All `multipart/form-data`; the PDF is the `file` field. The parser endpoints
also accept `pages`, `password`, and op-specific fields (`format`, `doc_type`,
`target_tokens`, `overlap`, `keep_furniture`).

> **Large documents.** Parse/chunk/extract-fields/info run **synchronously**,
> bounded by `OXIDE_REQUEST_TIMEOUT_SECS`, `OXIDE_MAX_FILE_SIZE`, and
> `OXIDE_MAX_PAGES`. The async **job queue** currently wraps `pdf2img` and
> `extract-images` (the long, output-heavy render jobs); see `docs/jobs.md`.
> Asynchronous parse/chunk submission is a planned extension — for very large
> parse jobs today, raise `OXIDE_REQUEST_TIMEOUT_SECS` accordingly.

### Docker

A 2-stage `Dockerfile` and a `docker-compose.yml` build and run `oxide-server`
(non-root user, `/health` healthcheck, `EXPOSE 8080`):

```sh
docker compose up --build
```

> **⚠️ The shipped `docker-compose.yml` will NOT start as-is.** It sets
> `OXIDE_API_KEYS=""` with no `OXIDE_ALLOW_UNAUTHENTICATED`, and the server is
> fail-closed — it refuses to boot without a key. **Before running**, edit the
> compose file (or your env) to set a real key:
>
> ```yaml
> environment:
>   OXIDE_API_KEYS: "your-strong-key-here"
>   OXIDE_CORS_ALLOWED_ORIGINS: "https://your-frontend.example.com"
> ```
>
> This non-booting default is intentional friction: it forces you to set a key
> rather than accidentally deploy an open API.

> **The default Docker image does NOT include OCR.** The `Dockerfile` builds
> `oxide-server` without the `ocr` feature and does not install `tesseract`. To
> self-host OCR-enabled extraction, build with the feature **and** install
> Tesseract in the runtime image (see §4).

### Configuration reference

`.env.example` documents **every** `OXIDE_*` variable with secure defaults — it
is the canonical config reference. Highlights:

| Variable | Default | Purpose |
| --- | --- | --- |
| `OXIDE_API_KEYS` | *(empty → fail-closed)* | Comma-separated valid API keys |
| `OXIDE_ALLOW_UNAUTHENTICATED` | `false` | Dev-only: run with NO auth |
| `OXIDE_CORS_ALLOWED_ORIGINS` | *(empty → none)* | Browser cross-origin allowlist |
| `OXIDE_RATE_LIMIT_PER_MIN` | `60` | Per-key requests/min (0 = off) |
| `OXIDE_MAX_FILE_SIZE` | `52428800` (50 MiB) | Max upload size |
| `OXIDE_MAX_PAGES` | `200` | Max pages per request |
| `OXIDE_REQUEST_TIMEOUT_SECS` | `30` | Cooperative per-request deadline |
| `OXIDE_MAX_RENDER_PIXELS` | `100000000` | Pixel-explosion guard |
| `OXIDE_MAX_OUTPUT_BYTES` | `2147483648` (2 GiB) | Output-size cap |
| `OXIDE_JOB_*` | *(various)* | Async job queue sizing/retention |

Deploy checklist (also in `.env.example` and `docs/security.md`): set strong
`OXIDE_API_KEYS`; set `OXIDE_CORS_ALLOWED_ORIGINS` to your frontend; size the
timeouts/limits to your workload; **terminate TLS in front** (Oxide speaks plain
HTTP behind a reverse proxy / load balancer).

---

## 3. Browser-side extraction (WASM)

For client-side extraction with **no server at all** (documents never leave the
browser tab), build the WASM package and use `OxidePdf`:

```sh
cargo build -p oxide-wasm --target wasm32-unknown-unknown --release
wasm-bindgen --target web \
  --out-dir crates/oxide-wasm/examples/browser/pkg \
  target/wasm32-unknown-unknown/release/oxide_wasm.wasm
# serve crates/oxide-wasm/examples/browser and open index.html
```

```js
const pdf = new OxidePdf(new Uint8Array(await file.arrayBuffer()));
const markdown = pdf.parseMarkdown();          // canonical model → Markdown
const chunkSet = JSON.parse(pdf.chunk(0, 0));   // RAG chunks (default 512/64)
const fields  = pdf.extractFieldsJson("auto");  // key-value fields
```

WASM is **digital-born only** — OCR needs the external Tesseract process and is
not available in the browser. See `crates/oxide-wasm/examples/browser/README.md`.

---

## 4. OCR (optional, external Tesseract)

OCR is an **opt-in build feature** that drives the external `tesseract` binary
(no linked C). To enable it:

1. **Install Tesseract** and the language packs you need:
   - Debian/Ubuntu: `apt-get install tesseract-ocr tesseract-ocr-eng` (add
     `tesseract-ocr-deu`, etc. for other languages).
   - macOS: `brew install tesseract tesseract-lang`.
   - Windows: install the UB-Mannheim Tesseract build and ensure `tesseract` is
     on `PATH`.
2. **Build with the feature**:
   ```sh
   cargo build --release -p oxide-cli --features ocr
   ```
3. **Use it** — scanned pages are recognized and flow through the same model as
   digital-born text:
   ```sh
   oxide parse          scanned.pdf --ocr --ocr-lang eng --format markdown
   oxide extract-fields scanned.pdf --ocr --type invoice
   oxide extract-text   scanned.pdf --ocr
   ```

For an OCR-enabled **server/Docker** image, extend the runtime stage of the
`Dockerfile` to `apt-get install -y tesseract-ocr tesseract-ocr-eng` and build
`oxide-server` with `--features ocr` (the server crate gains OCR the same way
the CLI does). Honest expectation: OCR quality is bounded by Tesseract and scan
quality; messy scans recover most text but key-value recall drops (see
`docs/parser_positioning.md`).

---

## 5. Example workflows

### Batch a folder of PDFs → Markdown for a local RAG/LLM

```sh
for f in docs/*.pdf; do
  oxide parse "$f" --format markdown -o "out/$(basename "$f" .pdf).md"
done
# Or chunk straight to JSON for an embedding pipeline:
for f in docs/*.pdf; do
  oxide chunk "$f" --target-tokens 512 -o "out/$(basename "$f" .pdf).chunks.json"
done
```

### Self-hosted invoice → JSON pipeline

```sh
# Digital-born invoices:
oxide extract-fields invoice.pdf --type invoice > invoice.json
# Scanned invoices (OCR build):
oxide extract-fields scan.pdf --ocr --type invoice > scan.json
```

### Private document API on your VPS

```sh
OXIDE_API_KEYS=$(openssl rand -hex 32) \
OXIDE_CORS_ALLOWED_ORIGINS=https://app.example.com \
  ./oxide-server     # behind nginx/Caddy terminating TLS
```

---

## 6. Resource guidance & privacy framing

- **Threads.** Text extraction and rendering parallelize across cores (rayon);
  the engine is shared via `Arc`, so per-page work scales without re-parsing.
- **Memory.** Render peak memory is roughly flat in page count (Arc-shared
  engine). The server's pixel-explosion guard, output-size cap, and image-count
  cap bound worst-case memory on untrusted input *before* allocation.
- **Timeouts.** The server uses cooperative cancellation: on the per-request /
  per-job deadline the engine's hot loops observe the cancel flag and bail,
  freeing the worker — it actually stops CPU-bound work rather than abandoning
  the wait. Tune `OXIDE_REQUEST_TIMEOUT_SECS` / `OXIDE_JOB_TIMEOUT_SECS`.
- **Privacy.** Everything runs on your hardware. Documents are never uploaded to
  a third party, there are no per-page cloud fees, and (for WASM) extraction can
  run entirely client-side. This is the thing teams pay Textract / Azure DI /
  Docling-cloud for — run it yourself, unmetered and private.

## See also

- `docs/parser_positioning.md` — honest wins/trails vs Docling/PyMuPDF/qpdf.
- `docs/parser_benchmark.md` — the reproducible extraction benchmark + numbers.
- `docs/security.md` — server security posture + deploy checklist.
- `docs/jobs.md` — async job API and its single-process/in-memory limitation.
- `docs/bindings.md` — C ABI and WASM embedding.
- `.env.example` — the complete `OXIDE_*` configuration reference.
