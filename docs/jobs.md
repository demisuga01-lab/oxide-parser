# Async Job API

The heavy endpoints (`pdf2img`, `extract-images`) have **asynchronous job
variants** under `/api/v1/jobs/...`. They exist for large/slow inputs that would
otherwise hold an HTTP connection open for the whole render/extract — fragile
across client/proxy/load-balancer timeouts, and capped outright by the
per-request timeout (`OXIDE_REQUEST_TIMEOUT_SECS`). A job runs in the background,
untied to a single request's lifetime: submit returns immediately with a job id,
the client polls status, then downloads the result when complete.

The synchronous endpoints (`POST /api/v1/pdf2img`, `POST /api/v1/extract-images`)
remain unchanged for small/fast jobs. **The async path is additive** — nothing
about the sync path changed.

## When to use sync vs async (explicit routing)

Routing is **explicit**: the client chooses. There is no automatic redirect or
size-based conversion (a possible future enhancement — see below).

- **Sync** (`/api/v1/pdf2img`): small/fast jobs — a handful of pages, expected to
  finish well within `OXIDE_REQUEST_TIMEOUT_SECS` (default 30s).
- **Async** (`/api/v1/jobs/pdf2img`): large jobs — many pages, high DPI, or any
  input you expect to take longer than the sync timeout. The per-job timeout
  (`OXIDE_JOB_TIMEOUT_SECS`, default 300s) is much larger.

> The `extract-images` **JSON metadata mode** (`Accept: application/json`) is
> lightweight (it never decodes image bytes) and is **sync-only**. The async
> variant always produces the ZIP.

## Endpoints

All four are **auth-gated** by the same middleware as the sync endpoints (API
key via `X-API-Key` or `Authorization: Bearer`).

### `POST /api/v1/jobs/pdf2img` · `POST /api/v1/jobs/extract-images`

Same multipart fields as the corresponding sync endpoint. Enqueues a job and
returns **202 Accepted**:

```json
{
  "job_id": "9f86d081884c7d659a2feaa0c55ad015",
  "status": "queued",
  "kind": "pdf2img",
  "status_url": "/api/v1/jobs/9f86d081884c7d659a2feaa0c55ad015",
  "result_url": "/api/v1/jobs/9f86d081884c7d659a2feaa0c55ad015/result"
}
```

If the queue is full (`OXIDE_JOB_QUEUE_CAPACITY`) or the store is at its cap
(`OXIDE_MAX_JOBS`), submission is rejected with **503 Service Unavailable** +
`Retry-After: 10` and `{"error":"queue_full"}`. This is backpressure — retry
shortly; it is distinct from 413 (input too large, retrying won't help).

### `GET /api/v1/jobs/{job_id}`

Poll status. **200 OK** with the current state:

```json
{
  "job_id": "...",
  "kind": "pdf2img",
  "status": "running",
  "progress": { "done": 7, "total": 20 }
}
```

- `status` is one of `queued`, `running`, `completed`, `failed`.
- `progress` appears once the total is known (when work begins). For pdf2img it
  counts pages encoded; it is best-effort and informational.
- On `completed`, a `result_url` field is included.
- On `failed`, `error` (a stable code) and `message` (a safe, non-leaking
  description) are included, plus a `reference` id for internal errors.

Unknown id, expired job, or **a job owned by a different API key** all return
**404** `{"error":"job_not_found"}` — see Ownership below.

### `GET /api/v1/jobs/{job_id}/result`

Download a completed job's output.

- **200 OK** with the result bytes and the same headers the sync endpoint sets
  (`Content-Type: application/zip`, `Content-Disposition`, and the `x-*` count
  headers). The result stays available for re-download until retention expiry.
- **409 Conflict** `{"error":"not_ready","status":"..."}` if the job is still
  `queued`/`running` — keep polling.
- **404** if unknown/expired/not owned by the requester.
- If the job `failed`, the classified error is replayed with an appropriate
  status (e.g. 422 for a malformed PDF, 503 for a timeout).

## Job lifecycle

```
        submit
          │
          ▼
      ┌────────┐   worker picks up   ┌─────────┐   success   ┌───────────┐
      │ queued │ ──────────────────▶ │ running │ ──────────▶ │ completed │
      └────────┘                     └─────────┘             └───────────┘
          │                              │                        │
          │ queue full → 503             │ error / timeout        │ retention TTL
          │ (not enqueued)               ▼                        ▼
          │                          ┌────────┐             ┌─────────┐
          └─────────────────────────▶│ failed │────────────▶│ reaped  │ (404)
                                      └────────┘  retention  └─────────┘
                                                     TTL
```

A completed or failed job is retained for `OXIDE_JOB_RETENTION_SECS` (default 1
hour) so the client can poll/download, then a background cleanup task drops the
job state and deletes its result file from disk. After that the id returns 404.

## Ownership & non-guessable ids

- Job ids are **128 bits of OS randomness**, hex-encoded (32 chars) — not
  enumerable.
- Every job records the **submitting identity** (the API key, or `anonymous`
  when running with `OXIDE_ALLOW_UNAUTHENTICATED`). Status and result are scoped
  to that identity.
- A request for a job owned by a **different** key returns **404, not 403**, so
  the endpoint never confirms the existence of another caller's job.

## Resource safety (bounded everything)

Consistent with the per-request safety model, the job system bounds every
dimension; nothing grows without limit:

| Bound                         | Config var                  | On breach |
|-------------------------------|-----------------------------|-----------|
| Queue length                  | `OXIDE_JOB_QUEUE_CAPACITY`  | 503 on submit |
| Worker concurrency            | `OXIDE_JOB_WORKERS`         | jobs wait in queue |
| Per-job wall-clock            | `OXIDE_JOB_TIMEOUT_SECS`    | job → failed (`timeout`) |
| Retained jobs (count)         | `OXIDE_MAX_JOBS`            | 503 on submit |
| Retention window              | `OXIDE_JOB_RETENTION_SECS`  | job reaped → 404 |
| Per-job output bytes          | `OXIDE_MAX_OUTPUT_BYTES`    | job → failed (`resource_limit`) |
| Render pixels / image count   | `OXIDE_MAX_RENDER_PIXELS` / `OXIDE_MAX_IMAGE_COUNT` | job → failed |

The per-job timeout reuses the **same cooperative cancellation** as the sync
path (the engine polls a cancel flag in its hot loops and bails, freeing the
thread). The per-job error is **classified and sanitized** exactly like the sync
path — no stack traces or internal paths leak to the client.

A single job's failure or panic never takes down a worker: the worker catches
the error, marks that job `failed`, and continues to the next.

## Output identical to sync (differential guarantee)

The async path **reuses the same engine entry points** as the sync handlers
(`process_pdf2img` / `process_extract_images`). For the same input it produces
byte-identical structure. The test suite proves this with a differential check:
a job's ZIP has the same page/entry count as the sync endpoint's ZIP for the
same request.

## Known limitation: in-memory, single-process

This implementation holds job state **in memory** and result files on **local
disk**, both keyed by job id. That means:

- Job state is **lost on restart** (in-flight and completed jobs vanish).
- It does **not scale horizontally** — a second instance has its own store and
  cannot serve another instance's job ids.

This is the intended scope: it delivers the async model with **zero external
dependencies** (no database, Redis, or message broker), matching the project's
lean, self-contained design. The [`JobStore`] trait is the seam — a persistent
or distributed backend can be implemented against it later without changing the
handlers or the worker.

## Possible future enhancements (not implemented)

- **Persistent/distributed backend** (DB / Redis / broker) for durability across
  restarts and multi-instance deployments.
- **Automatic sync→async routing**: the sync endpoint could detect oversized
  inputs (page count / size) and convert to a job, returning 202.
- **Streaming progress** via SSE or WebSocket instead of polling.

[`JobStore`]: ../crates/server/src/jobs/store.rs
