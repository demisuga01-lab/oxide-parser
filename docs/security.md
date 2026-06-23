# Oxide Server Security Posture

This document describes the server's security controls and how to deploy it
safely. The governing principle is **fail-closed**: a misconfiguration must
never silently leave the server in an unsafe state. Running open (no auth) or
permissive (any CORS origin) requires an **explicit, clearly-logged dev opt-in**.

See also [`robustness.md`](robustness.md) for resource-safety controls
(per-request timeout, pixel/output/image caps, pathological-input handling).
For full audit preparation, see the security packet under
[`security/`](security/): threat model, attack-surface map, crypto-review prep,
dependency policy, and audit-readiness checklist.
The consolidated code-level security and robustness posture is
[`security/posture.md`](security/posture.md).

## 1. Authentication — fail-closed API keys

API-key authentication is enforced by default.

- **Keys**: `OXIDE_API_KEYS` is a comma-separated list of valid keys. Clients
  present a key via either `X-API-Key: <key>` or `Authorization: Bearer <key>`.
- **Fail-closed startup**: if `OXIDE_API_KEYS` is empty **and**
  `OXIDE_ALLOW_UNAUTHENTICATED` is not set, the server **refuses to start**
  (logs a fatal error and exits non-zero). A forgotten key configuration fails
  loudly instead of silently exposing every endpoint.
- **Dev opt-in**: `OXIDE_ALLOW_UNAUTHENTICATED=true` is the explicit, dev-only
  escape hatch to run without keys. It logs a prominent warning on every
  startup. Never set it in production.
- **Constant-time comparison**: provided keys are checked against the
  configured allowlist with a constant-time byte comparison (`subtle::ct_eq`),
  comparing against every configured key without early-exit, so timing does not
  reveal how many leading bytes matched. This closes a key-guessing
  side-channel.
- **Status codes**: missing or invalid key → `401 Unauthorized` with a generic
  message. (There is no separate authz model, so 403 is not used.)
- **Health exemption**: `/health`, `/readiness`, `/api/v1/health`,
  `/api/v1/readiness` are intentionally exempt from auth so load balancers and
  orchestrators can probe them. They expose only liveness/version info.
- **Gated endpoints**: `/api/v1/extract-text`, `/api/v1/extract-images`,
  `/api/v1/analyze`, `/api/v1/pdf2img` (and `/api/v1/version`) require a valid
  key when auth is enforced.

## 2. CORS — restrictive by default

CORS is an allowlist, not the previous permissive (any-origin) policy.

- **Allowlist**: `OXIDE_CORS_ALLOWED_ORIGINS` is a comma-separated list of full
  origins (e.g. `https://app.example.com`). Only listed origins receive
  `Access-Control-Allow-Origin`.
- **Restrictive default**: with no origins configured, no cross-origin access
  is granted (effectively same-origin only) — the correct default for an
  auth-gated API that may handle sensitive documents.
- **Methods/headers**: only the methods the API serves (`GET`, `POST`,
  `OPTIONS`) and the headers a real client needs (`content-type`,
  `authorization`, `x-api-key`) are allowed, rather than "any".
- **Dev opt-in**: `OXIDE_CORS_ALLOW_ANY=true` allows any origin, mirroring the
  auth dev opt-in, and logs a startup warning. Local development only.
- Note: CORS is a browser-enforced control. It protects users' browsers from
  cross-site requests; it does not by itself protect the server from
  non-browser clients (those are handled by auth and rate limiting). A
  restrictive default is correct hygiene regardless.

## 3. Error responses — sanitized, with correlation IDs

Errors are classified so clients get useful, safe feedback while internal
detail never leaks.

- **Safe 4xx** (client-actionable): specific status + intentionally informative
  message that leaks no internals. Examples:
  - missing file → `400 missing_file`
  - invalid parameter → `400 invalid_parameter`
  - encrypted / wrong password → `422 encrypted`
  - no text layer → `422 no_text_layer`
  - malformed PDF → `422 malformed_pdf`
  - unsupported feature → `422 unsupported_feature` (names the feature; safe
    and useful)
  - request timed out → `503 timeout`
  - resource limit exceeded → `413 resource_limit`
- **Generic 500** (unexpected internal errors): the catch-all path returns only
  a generic message and a **correlation reference id** (`err-<hex>`). The full
  error detail is logged **server-side** keyed by that id
  (`tracing::error!(correlation_id, detail, …)`), so operators can debug from
  logs without exposing file paths, library internals, or stack-trace-like text
  to clients.
- This composes with the resource-safety errors: the specific timeout/limit
  responses remain specific 4xx/503 and are not swept into the generic path.

## 4. Rate limiting — bounded memory

A per-key sliding-window limiter (`OXIDE_RATE_LIMIT_PER_MIN`, 0 disables)
returns `429 Too Many Requests` with `Retry-After: 60` when a key exceeds its
limit within a 60-second window.

- **Scheduled cleanup**: a background task (spawned at startup, 60s interval)
  sweeps expired buckets via `cleanup_expired()`, so per-key state does not grow
  unbounded over the server's lifetime. The task holds a `Weak` reference and
  exits cleanly once the limiter (app) is dropped.
- **Active windows preserved**: cleanup removes only buckets whose window has
  fully elapsed; an active window is never reset prematurely.
- **Absolute backstop**: an upper cap on distinct tracked keys (100k) bounds
  worst-case memory even under adversarial key/IP rotation — when full, expired
  entries are swept and, if still full, the oldest window is evicted.
- **Testable clock**: the limiter is generic over a `TimeSource`; tests use a
  `ManualClock` to advance time deterministically and assert expiry/cleanup
  without real waiting.

## Deploying securely — checklist

1. **Set `OXIDE_API_KEYS`** to strong, unique key(s). Leave
   `OXIDE_ALLOW_UNAUTHENTICATED` unset/false.
2. **Set `OXIDE_CORS_ALLOWED_ORIGINS`** to your frontend origin(s). Leave
   `OXIDE_CORS_ALLOW_ANY` unset/false.
3. **Size the resource limits**: `OXIDE_REQUEST_TIMEOUT_SECS`,
   `OXIDE_MAX_FILE_SIZE`, `OXIDE_MAX_RENDER_PIXELS`, `OXIDE_MAX_OUTPUT_BYTES`,
   `OXIDE_MAX_IMAGE_COUNT`, `OXIDE_MAX_PAGES`, `OXIDE_MAX_DPI`.
4. **Set a rate limit** (`OXIDE_RATE_LIMIT_PER_MIN`) appropriate to your
   clients.
5. **Terminate TLS in front of the server** (reverse proxy / load balancer).
   Oxide speaks plain HTTP and is designed to sit behind one.

See [`.env.example`](../.env.example) for all variables with descriptions and
secure defaults.

## Remaining security follow-ups

- **Request/audit logging** of authenticated principals and per-request ids
  (the correlation id exists for 500s; extending it to all requests would aid
  audit).
- **TLS termination** is delegated to a fronting proxy; in-process TLS is not
  provided.
- **Per-endpoint authorization**: the current model is a single tier (valid key
  → full access). Scoped keys / per-endpoint authz could be layered on if
  multi-tenant separation is ever required.
- **Key rotation / hashing at rest**: keys are read from the environment in
  plaintext; a secrets manager and hashed-key storage would harden operations.
