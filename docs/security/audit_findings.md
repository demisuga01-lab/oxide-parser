# Oxide PDF SDK — Systematic Security Audit Findings

**Type:** Read-only, findings-only vulnerability assessment.
**Date:** 2026-06-23.
**Workspace:** `E:\wellpdfsdk` (6 crates, ~99k lines of Rust including tests).
**Scope/method:** Structured, prioritized hunt against the known high-risk
vulnerability classes, deepest in TIER 1 (crypto/signatures), then the
untrusted-input surface (TIER 2), then correctness-as-security (TIER 3). Every
finding is backed by quoted code at `file:line`. No source was modified.

> **Honest framing.** This is a *systematic and prioritized* review, **not an
> exhaustive proof of absence**. Absence of a finding in an area does not prove
> the area is bug-free. This review **complements but does not replace** a paid
> third-party human audit — especially for the crypto/signature math, timing
> side-channels, and subtle protocol-level signature attacks, where an AI
> read-only review is inherently weak (see §6). Where I could not fully assess
> something, it is stated plainly rather than implied as clean.

---

## Severity rubric

| Severity | Meaning in this report |
| --- | --- |
| **Critical** | Forged signature accepted as valid, private-key/plaintext-key leak, RCE, or memory corruption reachable from untrusted input. |
| **High** | DoS (OOM/hang/abort) reachable from an untrusted PDF; auth bypass; **data leak through a security feature** (redaction); a security verdict that is dangerously easy to misread as a stronger guarantee than it is. |
| **Medium** | Information leak; weaker-than-spec or unenforced crypto/permission control; integrity bug reachable only via crafted/damaged input; pre-1.0 crypto on a critical path. |
| **Low / Info** | Hardening gaps, defense-in-depth, latent fragility not currently reachable, or documented/accepted residual risk. |

---

## 1. Executive Summary

**Findings by severity:** 0 Critical · 6 High · 8 Medium · 12 Low/Info.

No issue was found that lets an attacker get a **forged** signature reported as
`Valid`, leak a private key, achieve RCE, or corrupt memory from untrusted PDF
bytes. The pure-Rust core holds: `unsafe` is genuinely confined to the C ABI
(verified), the parser's integer/offset arithmetic is checked, and the
cryptographic *primitives* (CSPRNG sourcing, constant-time verifiers, R2–R6 key
derivation, AES/RC4 paths) are carefully implemented.

The real risk is concentrated in three places:

1. **Redaction silently under-removes text (High, `editing.rs`).** The decision
   of *which glyphs fall under a redaction box* is computed from hard-coded
   fixed-width metrics (`0.5em` per glyph, `500` units per byte), not the real
   font widths or CID byte-width. For essentially any proportional or
   CID/CJK font the estimated glyph positions drift, so glyphs that are visually
   under the black box can test as *not* intersecting and their bytes are kept
   verbatim in the output content stream — recoverable by copy/paste or text
   extraction. The black mark is drawn independently, so the document *looks*
   redacted. Additionally, alternate text representations (inline
   `/ActualText` in marked content, the tagged-PDF structure tree, XMP, and
   attachments) are not scrubbed. Redaction is a security feature; its failure
   is a data leak.

2. **The signature `Valid` verdict means "cryptographically valid against the
   certificate embedded in the PDF" — not "trusted" or "authentic" (High,
   `signature.rs`).** There is no trust-chain validation to a configured root,
   no certificate validity-period enforcement, no key-usage check, and
   revocation/coverage are *not* verdict gates. A self-signed or
   attacker-controlled certificate over a malicious document yields
   `SignatureValidity::Valid`. This is honestly documented and the CLI labels it
   "cryptographically VALID" with a caveat note, but a programmatic integrator
   reading only `report.validity` can be badly misled.

3. **Image decoders allocate `width × height × channels` directly from
   attacker-controlled PDF dimension fields with no cap (High, `images/`).** A
   few-hundred-byte PDF declaring `/Width 60000 /Height 60000 /BitsPerComponent
   1` (or CCITT `/Columns`/`/Rows`, or a crafted JBIG2 region) forces a
   multi-gigabyte `Vec::with_capacity` and OOMs the process. The only pixel cap
   in the codebase is in the *render* layer, not the decode layer.

**Most important items (top 5):**
- **H-1** Redaction under-removes text via fake glyph metrics (`editing.rs:1142,1149`).
- **H-4 / H-5 / H-6** Unbounded image-decoder allocations → OOM (`decoder.rs:234`, `ccitt.rs:66`, `jbig2.rs:28`).
- **H-2** Redaction does not scrub alternate text representations (`editing.rs`, `/ActualText`, struct tree).
- **H-3** Signature `Valid` ≠ trusted; no chain/validity/revocation gating (`signature.rs:1005,1124`).
- **M-1** Unbounded page-tree recursion → stack-overflow abort (`document.rs:241`).

**Overall posture (one honest paragraph).** This is a security-conscious
codebase with strong, layered robustness engineering: pure-Rust memory safety,
checked parser arithmetic, fuzz/property/differential/corpus harnesses,
fail-closed constant-time server auth, no SSRF surface, and active content
(JS/Launch/auto-actions) that is parsed-and-rejected rather than executed — all
verified in code, not just asserted in docs. The cryptographic primitives are
done well. The gaps are not in the crypto math but in *application-level
security guarantees*: a redaction routine whose geometry is too crude to be
trustworthy for its stated purpose, a signature-verification API whose `Valid`
verdict promises less than its name implies, and an image-decode path missing
the resource caps that the rest of the parser has. None of these are
memory-safety holes; the redaction and signature items are the ones that could
cause a user to trust a document they should not. A paid third-party
cryptography/signature audit remains the highest-value next step, exactly as the
project's own `posture.md` states.

---

## 2. TIER 1 — Cryptography & Signatures (deepest review)

Files read in full by the auditor: `crates/engine/src/crypto.rs` (2,223 lines)
and `crates/engine/src/signature.rs` (1,726 lines), plus the reader integration
in `crates/engine/src/reader.rs:445-582`.

### H-3 — `SignatureValidity::Valid` is not a trust/authenticity verdict
- **Severity:** High
- **Location:** `signature.rs:1005-1017` (`scope_note`), `signature.rs:1102-1133`
  (`verify_cms` final RSA step), `signature.rs:1284-1305` (`find_signer_cert`),
  `signature.rs:938-1003` (`verify_one`, coverage computed but never gates
  validity), `signature.rs:855-882` (`revocation_status_from_crls`, reported but
  never gates validity).
- **Class:** 2.4 — signature validation rigor.
- **Description / trust boundary.** A signed PDF is untrusted. `verify_cms`
  establishes `Valid` iff (a) the `messageDigest` signed attribute equals the
  digest of the `/ByteRange` bytes and (b) the RSA signature over the signed
  attributes verifies against the public key of a certificate **taken from the
  PDF's own CMS blob**. There is no verification that the certificate chains to a
  configured trust anchor, no `notBefore`/`notAfter` enforcement, no
  KeyUsage/EKU check, and revocation status — even
  `RevocationStatus::RevokedByEmbeddedCrl` — does not change the verdict. The
  `coverage` field (`WholeFile` vs `ModifiedAfterSigning`) is likewise a
  *separate* field, not a gate on `validity`.
- **Exploitability.** An attacker generates a self-signed RSA key/cert, signs an
  arbitrary malicious document, embeds the cert in the CMS, and `verify_signatures`
  returns `validity: Valid`. This is *not* a cryptographic forgery (it requires
  the attacker's own private key and cannot impersonate a specific trusted
  signer's key), but a consumer that treats `Valid` as "this document is
  authentically signed by a trusted party" is defeated. Realistic and reachable
  for any integrator who reduces the report to its `validity` field. **Honest
  mitigations already present:** the module docs (`signature.rs:14-25`) and
  `scope_note` state this scope explicitly; the CLI prints "Signature is
  cryptographically VALID", shows coverage/revocation on separate lines, and
  prints the caveat `note` (`cli/src/main.rs:2016-2091`). The danger is at the
  programmatic API surface, not the CLI.
- **Evidence.**
  ```rust
  // signature.rs:1005 — the verdict's own scope note spells out the gap:
  "Trust-chain validation to a root CA, certificate validity-period \
   enforcement, OCSP response policy validation, and timestamp imprint/TSA trust are \
   not verdict gates"
  // signature.rs:1284 — signer cert is whatever the PDF supplies, fallback to first:
  fn find_signer_cert(signed: &SignedData, signer: &SignerInfo) -> Option<Certificate> { ... first }
  // signature.rs:975 — coverage is recorded, never gates validity:
  report.coverage = compute_coverage(&byte_range, file.len());
  ```
- **Suggested direction (do not implement here).** Add optional trust-anchor
  configuration and, when provided, chain + validity-period + KeyUsage
  validation, and make revocation and `coverage != WholeFile` downgrade the
  verdict (or add an explicit composite `trusted: bool` distinct from
  `cryptographically_valid`). At minimum, document at the *type* level (rustdoc
  on `SignatureValidity::Valid`) that it is cryptographic-only, so programmatic
  consumers cannot miss it.

### M-2 — Incomplete `/ByteRange` coverage validation
- **Severity:** Medium
- **Location:** `signature.rs:1605-1628` (`extract_signed_bytes`, `compute_coverage`).
- **Class:** 2.4 — signature ByteRange integrity (the classic partial-coverage / "shadow" attack class).
- **Description.** Best practice is that a PDF signature's `/ByteRange = [a b c d]`
  must satisfy `a == 0` (covers from the file start), the excluded gap `[b..c]`
  must be **exactly** the `/Contents` hex string and nothing else, and
  `c + d == filesize`. Oxide validates only the last condition, and even that
  with a 3-byte slack:
  ```rust
  // signature.rs:1620
  fn compute_coverage(br: &ByteRange, file_len: usize) -> Coverage {
      let signed_end = br.c.saturating_add(br.d);
      if signed_end + 3 >= file_len { Coverage::WholeFile } else { Coverage::ModifiedAfterSigning }
  }
  ```
  `extract_signed_bytes` enforces only ordering/bounds (`br.c >= br.a + br.b`,
  ranges in-file); it does **not** require `a == 0` nor that the gap equals the
  `/Contents` region. A signature could therefore leave a prefix
  (`file[0..a]`) or an oversized interior gap unsigned while still reporting
  `WholeFile`.
- **Exploitability.** Limited in the most common threat (a third party tampering
  with an already-signed document) because the `/ByteRange` array lives *inside*
  the signed range 1, so an attacker cannot rewrite it without invalidating the
  signature. The residual risk is in signer-controlled or
  re-saved/incrementally-updated documents (shadow-attack variants and
  repudiation scenarios), where partial coverage is not flagged. Rated Medium:
  a real rigor gap in the highest-stakes logic, partially mitigated by the
  ByteRange itself being signed.
- **Suggested direction.** Reject (or at least downgrade coverage for) any
  signature whose first range does not start at offset 0 or whose excluded gap
  does not coincide with the located `/Contents` string; tighten the `+3` slack
  to a precise trailing-whitespace check.

### L-1 — SHA-1 accepted for signature digests on par with SHA-256
- **Severity:** Low
- **Location:** `signature.rs:1189-1194` (`verify_rsa` SHA-1 arm),
  `signature.rs:1200-1208` (`digest_bytes`), OIDs `signature.rs:54`,`1145`.
- **Class:** 2.4 — algorithm strength.
- **Description.** SHA-1 (`OID_SHA1`, `OID_SHA1_RSA`) is accepted and treated
  identically to SHA-256/384/512. SHA-1 is collision-broken; a SHA-1-based
  signature is far weaker and, in principle, forgeable via chosen-prefix
  collisions. The digest algorithm is surfaced in `report.digest_algorithm`, so
  it is observable, but it does not downgrade the verdict or emit a warning.
- **Exploitability.** Low/theoretical for the average integrator (constructing a
  usable PDF SHA-1 collision is expensive), but it should not be silently
  blessed as `Valid` with no signal.
- **Suggested direction.** Keep SHA-1 *parsing* for interop reporting, but flag
  SHA-1 signatures as weak (a distinct status or a forced note), and consider
  refusing them by default with an opt-in.

### L-2 — RSA signing carries the Marvin timing advisory (RUSTSEC-2023-0071)
- **Severity:** Low (documented/accepted)
- **Location:** `signature.rs:1517,1553` (signing via `SigningKey::<Sha256>`),
  `Cargo.lock` `rsa 0.9.10`; verification at `signature.rs:1162-1192` uses the
  **public** key.
- **Class:** 2.6.
- **Description.** `rsa 0.9.10` is affected by the Marvin PKCS#1 v1.5 timing
  side-channel. **Verification** (the operation performed on untrusted input)
  uses only the public key and is unaffected. **Signing** uses the private key;
  the Marvin attack requires many timed operations against attacker-influenced
  inputs. In an offline SDK the signer signs locally with its own key, so
  practical exposure is low; it becomes relevant only if RSA signing is exposed
  as a remotely-timed signing oracle. No RSA *decryption* oracle exists (PDF
  encryption uses AES/RC4, not RSA).
- **Status.** Already an explicit, documented `cargo-deny` exception
  (`deny.toml:7`, `crypto_review.md`). Confirmed reachable but gated by
  deployment model. (Cross-ref M-8.)
- **Suggested direction.** Prefer ECDSA where possible; do not expose RSA signing
  as a network-timed oracle; revisit when a fixed pure-Rust `rsa` release ships.

### L-3 — V5 `/Perms` integrity is not enforced; PDF `/P` permissions are advisory
- **Severity:** Low
- **Location:** `crypto.rs:963-973` (`verify_v5_perms` checks only the
  `'a','d','b'` magic at bytes 9–11), `reader.rs:556-561` (`/Perms` failure
  logs a warning and proceeds).
- **Class:** 2.5 — permissions actually enforced where claimed.
- **Description.** ISO 32000-2 §7.6.4.4 specifies that after deriving the file
  key the decrypted `/Perms` block should be checked so that bytes `[0..4]`
  equal `/P` and byte `[8]` matches `EncryptMetadata`, detecting tampering with
  the permission dictionary. Oxide checks only the `adb` magic, and even a magic
  mismatch is non-fatal:
  ```rust
  // reader.rs:559
  if !verify_v5_perms(&file_key, info) {
      log::warn!("V5 /Perms magic-byte check failed; proceeding with derived key");
  }
  ```
  Separately, the SDK does not enforce the `/P` permission bits (print/copy/modify)
  anywhere — once the file key is derived, full content access is granted. This
  is *standard* for PDF libraries (permissions are advisory and a
  consuming-application responsibility), and the writer defaults to
  `permissions: -1`. The finding is that integrators must not assume the SDK
  enforces document permissions, and that `/Perms` tampering is not surfaced.
- **Exploitability.** Low — permission flags are advisory; the file-key holder
  has full access regardless. No confidentiality bypass.
- **Suggested direction.** Validate the full `/Perms` plaintext against `/P` and
  `EncryptMetadata` and surface a tamper warning; document explicitly that `/P`
  is not enforced by the SDK.

### L-4 — Key material is not zeroized (returned keys, R6 intermediates, passwords)
- **Severity:** Low (documented residual)
- **Location:** `crypto.rs:354-409` (`r6_hash` builds `seed`, `k`, `k1` holding
  the password repeated 64×, none zeroized), `crypto.rs:911-957`
  (`derive_v5_file_key_from_*` return plain `Vec<u8>` file keys),
  `crypto.rs:1192-1205` (`EncryptParams { user_password, owner_password:
  Vec<u8> }`).
- **Class:** 2.2 — key handling / zeroization.
- **Description.** The password-verifier *temporaries* are correctly wiped with
  `.fill(0)` (`crypto.rs:810,819,827,875,903`), but derived file keys returned
  to callers, the R6 hash intermediates, and the caller-supplied password
  buffers linger in heap memory until dropped (and `Vec` drop does not zero).
  Notably, `zeroize 1.8.2` is **already in the dependency tree** but is not used
  for these buffers.
- **Exploitability.** Low; requires a local memory-disclosure primitive (core
  dump, swap, another process reading memory). Consistent with the documented
  "returned key material still uses ordinary `Vec<u8>`" limitation.
- **Suggested direction.** Wrap file keys / intermediates / `EncryptParams`
  password fields in `zeroize::Zeroizing<Vec<u8>>` (the crate is already
  available), or `.fill(0)` the R6 `k`/`k1`/`seed` before they drop.

### Crypto — verified clean (no issue found after review)
- **CSPRNG sourcing (2.1).** `random_bytes` delegates to `getrandom`
  (`crypto.rs:1057-1061`). All IVs (`aes128_cbc_encrypt_pkcs7:1119`,
  `aes256_cbc_encrypt_pkcs7:1147`), salts (`build_v5:1376-1395`), the V5 file key
  (`build_v5:1371`), and the `/Perms` random tail (`build_v5_perms:1433`) come
  from it. **No IV reuse:** CBC string/stream encryption uses a fresh random
  16-byte IV prepended to each ciphertext; the V5 `/UE`//OE` wrap uses the
  spec-required zero IV over a *unique* salt-derived key (correct, not reuse).
- **Constant-time comparison (2.3).** `ct_eq` uses `subtle::ConstantTimeEq`
  (`crypto.rs:723-725`) and gates all password-verifier comparisons
  (`verify_user_password:818,823`, `verify_v5_user_password:874`,
  `verify_v5_owner_password:902`).
- **Key derivation correctness (2.5).** R2–R6 derivation matches the spec and is
  round-trip tested (`crypto.rs:1781-1971`); the R6 iterated hash has a bounded
  safety cap (`round >= 256` guard, `crypto.rs:401`) and the password is
  truncated to 127 bytes (`truncate_v5_password:839`), so it cannot be driven to
  unbounded work. Password verification gates key derivation in the reader
  (`reader.rs:472-495,533-573`).
- **No padding-oracle exposure.** AES decryption failure returns the input bytes
  unchanged rather than signaling a padding error
  (`decrypt_string:1022,1026`), and the server sanitizes errors (see §3), so no
  distinct padding-error oracle is exposed.

### Signature — verified clean
- No code path turns a digest/messageDigest mismatch or a bad RSA verification
  into `Valid`: a `messageDigest` mismatch returns `Invalid`
  (`signature.rs:1091-1100`), unsupported algorithms return
  `UnsupportedAlgorithm` (fails closed, not `Valid`), and a missing/wrong cert
  yields `Invalid`/`Error`. Substituting a different embedded cert cannot turn
  `Invalid` into `Valid` (the RSA check then fails). The signed-attributes
  re-encoding hack (`reencode_signed_attrs_as_set:1264-1280`) can only cause a
  *false negative* (Invalid), never a false positive.
- No panic path found in `verify_cms`/`verify_one`: empty `signer_infos` is
  handled (`:1043`), all parses are `?`/`map_err` into a classified `Error`
  verdict.

---

## 3. TIER 2 — Untrusted-Input Attack Surface

### M-1 — Unbounded page-tree recursion → stack-overflow abort
- **Severity:** Medium
- **Location:** `document.rs:191` (`walk_page_tree`), recursive self-call at
  `document.rs:241`.
- **Class:** 3.1 — unbounded recursion.
- **Description.** `walk_page_tree` recurses into each `/Kids` entry. The only
  guard is a `visited: HashSet<u32>` keyed by object *number*
  (`document.rs:226`), which prevents *cycles* but does not bound recursion
  *depth*. A page tree that is a long linear chain of distinct `/Pages` nodes
  (each `/Kids` = `[next 0 R]`) recurses one stack frame per node. Unlike the
  parser (`parser.rs:18` `MAX_PARSE_DEPTH = 64`), the reference resolver
  (`reader.rs:364`, depth > 64), and PostScript functions, this path has **no
  depth cap**.
- **Exploitability.** Reachable via `PdfDocument::get_pages()` (the `parse` entry
  point and most page operations). A crafted file with tens of thousands of
  distinct nested `/Pages` nodes (cheap via object streams) overflows the native
  stack, which in Rust **aborts** the process (not a catchable error). Rated
  Medium because it needs a sizeable crafted file (depth bounded by object
  count, bounded by file size), not a tiny one.
- **Evidence.**
  ```rust
  // document.rs:226
  if !visited.insert(kid_ref.0) { /* cycle skip */ continue; }
  // document.rs:241 — no depth parameter, no cap:
  self.walk_page_tree(kid_ref, kid_dict, inherited.clone(), visited, pages)?;
  ```
- **Suggested direction.** Add an explicit depth counter and bail past a generous
  cap (matching the depth-64 convention), or convert to an iterative work-stack.

### M-3 — Image bit-depth normalization allocates from unbounded PDF dimensions (OOM)
- **Severity:** High
- **Location:** `images/decoder.rs:234,244,255` (`normalise_bit_depth`, bpc
  4/2/1), size from `expected_len` (`decoder.rs:648-650`).
- **Class:** 3.1 — unchecked allocation sized from a file field.

> *Numbered as a High finding (H-4) in §1; kept here in TIER-2 order.*

- **Description.** `Vec::with_capacity(width as usize * height as usize *
  channels as usize)` is driven directly by the PDF `/Width`/`/Height` with no
  upper bound (`images/locator.rs` reads dims as `.max(0) as u32`). `with_capacity`
  reserves the full size immediately.
- **Exploitability.** A few-hundred-byte stream with `/Width 60000 /Height 60000
  /BitsPerComponent 1 /DeviceRGB` reserves ~10.8 GB → OOM. Reached from
  `ImageDecoder::decode` (`decoder.rs:103-111`) and `decode_inline`
  (`decoder.rs:162-170`); reachable from rendering, `pdf2img`, and the server
  `extract-images` endpoint. The render-layer pixel cap
  (`OXIDE_MAX_RENDER_PIXELS`) does **not** gate embedded-image decode. (bpc 8/16
  are sized from the actual `raw` buffer — safe.)
- **Evidence.**
  ```rust
  // decoder.rs:234-235
  let total = expected_len(width, height, channels);
  let mut out = Vec::with_capacity(total);
  // decoder.rs:648
  fn expected_len(width: u32, height: u32, channels: u8) -> usize {
      width as usize * height as usize * channels as usize   // unchecked, unbounded
  }
  ```
- **Suggested direction.** `checked_mul` the dimension product and reject above a
  configurable pixel cap *before* allocating; share one cap across the decode
  layer.

### M-4 — CCITT decode sink pre-allocates `columns × rows` from DecodeParms (OOM)
- **Severity:** High
- **Location:** `images/ccitt.rs:65-71` (`GrayscaleSink::new`), params from
  `ccitt_decode_params` (`decoder.rs:665-686`).
- **Class:** 3.1.

> *Numbered H-5 in §1.*

- **Description / exploitability.** `Vec::with_capacity(width as usize * height
  as usize)` with `/Columns`/`/Rows` attacker-controlled and capped only at
  `u32::MAX`. `/Columns 100000 /Rows 100000` reserves ~10 GB before any fax data
  is decoded. Reached from `decode_inline` (`decoder.rs:148`) and
  `decode_remaining_image_filter` (`decoder.rs:319-323`).
- **Evidence.** `ccitt.rs:66` `let expected = width as usize * height as usize;`
  → `Vec::with_capacity(expected)`.
- **Suggested direction.** Bound `columns*rows` (with `checked_mul`) before
  constructing the sink.

### M-5 — JBIG2 decode sink pre-allocates from codestream-declared dimensions (OOM)
- **Severity:** High (residual depends on whether `hayro_jbig2` caps region dims internally — treat as exploitable until confirmed)
- **Location:** `images/jbig2.rs:28-34` (`GrayscaleSink::new`), dims from
  `image.width()/height()` (`jbig2.rs:14`).
- **Class:** 3.1.

> *Numbered H-6 in §1.*

- **Description / exploitability.** Same `Vec::with_capacity(width*height)`
  pattern; JBIG2 region fields are 32-bit, so a crafted stream can declare a
  multi-gigapixel page forcing a giant reservation. Reached from
  `decoder.rs:152,325-328`.
- **Suggested direction.** Validate decoder-reported dims against a cap before
  allocating.

### M-6 — `expected_len` integer overflow / forced `resize`
- **Severity:** Medium
- **Location:** `images/decoder.rs:648-650` (`expected_len`), consumed at
  `decoder.rs:234,244,255,391,464,661` (`resize_mismatch` → `pixels.resize`).
- **Class:** 3.1 / 3 (integer overflow).
- **Description.** `width * height * channels` uses unchecked `*`. On 64-bit
  `usize` it cannot wrap but is unbounded (feeds M-3 and a forced full-size
  `resize` at `decoder.rs:661`). On a 32-bit target (e.g. wasm32) it can wrap to
  a small value, after which `resize_mismatch` pads/truncates to a wrong size,
  leaving `RawImage.pixels.len()` inconsistent with the advertised dimensions
  that downstream consumers may trust.
- **Suggested direction.** `checked_mul` chain returning a decode error on
  overflow/over-cap.

### L-5 — Only FlateDecode is decompression-bomb capped; LZW/RunLength outputs are uncapped
- **Severity:** Low
- **Location:** `filters.rs:18` (`MAX_FLATE_DECOMPRESSED_BYTES = 512 MiB`, with a
  correct `.take()`-gated streaming guard at `filters.rs:314-339`); uncapped
  decoders: `lzw_decode:714`, `run_length_decode:676`, `ascii85_decode:582`,
  `ascii_hex_decode:555`; chain loop `decode_stream_parts:151`.
- **Class:** 3.1 — decompression bomb.
- **Description.** Only `FlateDecode` enforces an output ceiling. `RunLength` can
  expand ~128× (`filters.rs:704-705`) and LZW output grows unbounded (the
  dictionary is bounded to 4096 entries, but `out` is not). Because the input to
  these decoders is itself bounded by file size (and a preceding Flate stage is
  capped), this is not a tiny-file bomb, but a multi-hundred-MB input could
  expand to an OOM through RunLength.
- **Suggested direction.** Apply a shared cumulative output-size ceiling across
  the whole filter chain in `decode_stream_parts`.

### L-6 — Sync server ingest runs engine parse outside panic-isolation / deadline
- **Severity:** Low
- **Location:** `routes/analyze.rs:70-81`, `routes/extract_text.rs:135-187`,
  `routes/pdf2img.rs:72-114`, `routes/extract_images.rs:61-147`,
  `routes/parse_ops.rs:108-128`.
- **Class:** 3.5 — hostile upload.
- **Description.** The PDF open/parse phase (the most panic-prone code) runs
  directly on a tokio worker for the synchronous endpoints — not inside
  `spawn_blocking` and not under `catch_unwind`, and several handlers do not wrap
  the open phase in the request deadline. The async **job worker** *is* isolated
  (`jobs/worker.rs:208-224`, panic → `JoinError` → classified failure), so this
  gap is sync-path only.
- **Exploitability.** Low with the default `panic = "unwind"`: a parser panic
  unwinds into the per-connection task and drops that connection without
  crashing the process. Two residual concerns: a slow-but-not-panicking parse
  occupies a runtime worker thread without cooperative cancellation, and **if
  `panic = "abort"` were ever added to a release profile this becomes a High
  remote crash**.
- **Suggested direction.** Move the open/`page_count`/locate phase into the same
  `spawn_blocking`/deadline envelope the job path uses, and/or `catch_unwind`
  the engine entry points; pin `panic = "unwind"` explicitly.

### L-7 — Rate limiter collapses to a single global bucket without auth
- **Severity:** Low
- **Location:** `rate_limit.rs:204`, `auth.rs:42-44`.
- **Class:** 3.5 — rate-limit enforcement.
- **Description.** The limiter keys on the API key, falling back to the literal
  `"anonymous"` when none is present; there is no per-IP component. With
  `OXIDE_ALLOW_UNAUTHENTICATED=true` every caller shares one bucket (one abuser
  starves all dev traffic). When auth is enabled (production default),
  unauthenticated requests are rejected by the outermost auth layer first, so
  this only bites in dev mode.
- **Suggested direction.** Key the limiter on peer IP (`ConnectInfo`) when no API
  key is present, or document that rate limiting is per-key only.

### L-8 — JSON extract-text leaks raw per-page engine error strings
- **Severity:** Low (info leak)
- **Location:** `routes/extract_text.rs:176-185`.
- **Class:** 3.5 — info leak in error responses.
- **Description.** In the `output_format == "json"` branch a per-page failure is
  serialized as `"error": e.to_string()`, bypassing the central
  `ServerError`/`ClassifiedError` sanitization (`error.rs:159-174`). Whatever the
  engine error `Display` emits (object numbers, offsets, internal feature names,
  possibly inner I/O detail) is returned verbatim. Bounded by what the engine
  error prints (PDF-structural detail, not host secrets), so Low.
- **Suggested direction.** Route per-page errors through the existing sanitizer
  and return only `error_code`/sanitized `message`.

### L-9 — Job result files written to shared temp without restrictive permissions
- **Severity:** Low
- **Location:** `jobs/worker.rs:164-177` (`resolve_result_dir`), `:298-314`
  (`persist_result`).
- **Class:** 3.5 / 3.4 — data exposure at rest.
- **Description.** Completed job outputs are written to
  `std::env::temp_dir()/oxide-jobs-<pid>/<job_id>.bin` via `std::fs::write` with
  default permissions. Job IDs are unguessable (good — protects HTTP
  enumeration), but on a shared host any local user who can read the system temp
  dir can read other tenants' outputs directly off disk. No `0700`/`0600` mode is
  set.
- **Exploitability.** Requires local filesystem access (co-tenant), not a network
  attacker.
- **Suggested direction.** Create the dir `0700` and files `0600` (or use a
  tempfile crate with restrictive perms); document at-rest persistence.

### L-10 — OCR temp file uses a predictable, non-`O_EXCL` name
- **Severity:** Low
- **Location:** `oxide-ocr-tesseract/src/lib.rs:253-265` (`TempPgm::write`).
- **Class:** 3.4 / 3.7 — temp-file race / symlink.
- **Description.** The temp path is
  `std::env::temp_dir()/oxide-ocr-{pid}-{seq}.pgm`, created with `File::create`
  (`O_CREAT|O_TRUNC`, **not** `O_EXCL`) and a fully predictable name (PID + a
  process-global counter). On a multi-user host with a shared temp dir, a local
  attacker who pre-creates the path as a symlink gains a truncate/overwrite
  primitive against a victim-owned file. Not remotely reachable and not
  triggerable by PDF content; cleanup is correct (RAII `Drop`).
- **Suggested direction.** Use the `tempfile` crate (or `O_EXCL` + random suffix)
  so creation fails closed if the path exists.

### Info — C-ABI free functions are outside `catch_unwind`
- **Severity:** Info (latent, currently unreachable)
- **Location:** `oxide-capi/src/lib.rs:81,94,107,118` (the four `*_free` fns).
- **Class:** 3.6.
- **Description.** The 8 data-returning C-ABI functions and `*_open_from_bytes`
  are wrapped in `catch_unwind`, but the four free functions are not. A panic in
  a `Drop` during a free would unwind across the C boundary (UB). **Verified
  mitigant:** there are *zero* `impl Drop` in `crates/engine/src`, so no panic
  path exists today; this is latent fragility, not a live bug.
- **Suggested direction.** Set `panic = "abort"` for the cdylib/staticlib
  profile, or wrap the free bodies in `catch_unwind` too.

### TIER 2 — verified clean (high-value positives)
- **No SSRF surface (3.2).** Workspace-wide search for HTTP clients
  (`reqwest`/`hyper::client`/`ureq`/`TcpStream::connect`/`url::Url`/`fetch`)
  found none. The only socket is the server's inbound `TcpListener`
  (`server/src/main.rs:51-52`). `/Link` URIs are extracted as markdown text
  only (`engine.rs:834-851`); signature OCSP/CRL/TSA material is read only from
  bytes embedded in the PDF or supplied by the caller, never fetched
  (`signature.rs:22-23,153-157,251-252`).
- **Active content never executed (3.3).** JavaScript/`/JS`/`/AA`/`OpenAction`/
  `/Launch` are detected as PDF/A violations (`compliance.rs:810-819`) and
  *stripped* by the sanitizer (`compliance.rs:1077-1086`); the name-tree walker
  only collects pairs (`attachments.rs:310-359`). No interpreter or action
  dispatcher exists in the codebase. No `Command`/`spawn` is reachable from PDF
  parsing (the only production subprocess is the explicit OCR backend).
- **Path traversal blocked (3.4).** `sanitize_filename` (`attachments.rs:271-296`)
  strips path separators, drive letters, control chars, and `..`, and is
  actually invoked before the write (`cli/src/main.rs:1935-1937`); covered by
  traversal unit tests.
- **OCR command injection blocked (3.7).** The backend is invoked via
  `Command::new(binary).args(vec)` with positional argv — no shell, no string
  interpolation (`oxide-ocr-tesseract/src/lib.rs:181-182`). Untrusted language
  codes/paths are single argv elements. Bounded by a 60s timeout with drained
  pipes (`:178-240`).
- **C-ABI memory safety (3.6).** Null checks on every pointer
  (`checked_doc:380`, per-fn out-pointer checks), `catch_unwind` on all
  data-returning entry points, paired `Box`/`CString`/buffer ownership transfer,
  and input copied before parse (`to_vec()` at `lib.rs:59`). WASM exposes no
  fs/network capability.
- **Parser arithmetic (3.1).** Object-stream `/N` preallocation is capped and
  regression-tested (`reader.rs:1079`); xref-stream and classic-xref offset/size
  arithmetic use `checked_add`/`try_from` with bounds checks
  (`reader.rs:950-1057,849-919`); xref `/Prev` chains use offset-keyed cycle
  detection (`reader.rs:710-738`); reference resolution is depth-64 + visited-set
  bounded (`reader.rs:358-371`).
- **Server controls.** Fail-closed, router-wide, constant-time API-key auth as
  the outermost layer (`app.rs:148-151`, `auth.rs:54-68`,
  `main.rs:20-24`); body-size/DPI/page/pixel/output caps on ingest; worker panic
  isolation + `CancelToken` cooperative cancellation; unguessable 128-bit job
  IDs + owner scoping + uniform 404 (`jobs/id.rs`, `routes/jobs.rs`); bounded
  queue/store/rate-limit map; restrictive default CORS (`app.rs:198-200`);
  central error sanitization that never echoes internal detail
  (`error.rs:159-174`).

---

## 4. TIER 3 — Correctness-as-Security, `unsafe` Inventory, Decoders, Dependencies

### H-1 — Redaction under-removes text via fixed-width glyph metrics (data leak)
- **Severity:** High
- **Location:** `editing.rs:1149-1156` (`glyph_rect`, fixed `font_size * 0.5`),
  `editing.rs:1141-1147` (`advance_text` = `bytes * 500.0`), consumed by
  `redact_string_bytes` (`editing.rs:1378-1421`, per-byte intersection test at
  `:1390-1393`).
- **Class:** 4.1 — redaction true-removal.
- **Description / trust boundary.** Redaction correctly rewrites the content
  stream (it does *not* merely paint a box), but the decision of *which glyphs
  fall under the redaction rectangle* is computed from hard-coded metrics, not
  the actual font:
  ```rust
  // editing.rs:1149
  fn glyph_rect(&self, index: usize) -> ImageRect {
      let char_width = self.font_size * 0.5;            // every glyph assumed 0.5em wide
      let x0 = self.text_matrix[4] + index as f64 * char_width;
      ...
  }
  // editing.rs:1142
  fn advance_text(&mut self, bytes: usize) { self.advance_text_units(bytes as f64 * 500.0); }
  ```
  `redact_string_bytes` iterates *bytes*, computes a `glyph_rect` per byte index,
  and keeps any byte whose estimated rect does not intersect the redaction rect —
  re-emitting it verbatim into the output `TJ` array. Real glyph advances vary
  widely (a "W" or CJK glyph ≫ 0.5em; "i"/"l"/space ≪ 0.5em), and multi-byte CID
  fonts use ≥2 bytes per glyph while the code treats each byte as one 0.5em
  glyph. So estimated positions drift from true positions, and a glyph that is
  *visually* under the black box can test as non-intersecting and survive in the
  output stream — recoverable by copy/paste or text extraction. The black mark
  (`write_redaction_mark`, `editing.rs:1473`) is drawn from the operator's rect
  independently, so the document *looks* redacted.
- **Exploitability.** Realistic and reachable, not gated. Affects essentially any
  document whose fonts are not ~0.5em fixed-width — i.e. nearly all real
  proportional and CID/CJK fonts. Drift accumulates with glyph index, so the
  late glyphs of a long redacted run and boundary glyphs are the most likely to
  leak; CID fonts can fail wholesale. The passing tests use Helvetica at small
  offsets where 0.5em is coincidentally close enough and do not exercise
  wide/narrow or CID fonts. *(Verified firsthand against the quoted code.)*
- **Suggested direction.** Drive glyph rects from the resolved font's real
  `/Widths`//W` arrays and CID byte-width (the engine already resolves font
  widths in `fonts/resolver.rs`). Where metrics are unavailable, **fail closed**:
  drop the entire show-string whose run intersects, rather than keeping bytes.

### H-2 — Redaction does not scrub alternate text representations
- **Severity:** High (exploitability conditional on document structure — see below)
- **Location:** content rewriter operator match `editing.rs:1283-1324` (no
  `BDC`/`BMC`/`DP` handling; `_ =>` re-serializes verbatim at `:1320`);
  `/ToUnicode`, `/StructTreeRoot`, and XMP are absent from `editing.rs`
  entirely; XMP scrub gap at `scrub_pdf_strings` (`editing.rs:2731`, the
  `Stream { dict, .. }` arm drops the raw payload).
- **Class:** 4.1 — redacted content surviving in an alternate representation.
- **Description.** Even when the visible glyph bytes are removed, the same text
  can persist elsewhere and is not addressed:
  - **Inline `/ActualText` / `/Alt` in marked content** (`/Span <</ActualText
    (secret)>> BDC … EMC`) is re-serialized verbatim by the content rewriter,
    which only handles `Tj/TJ/'/"`, paths, and `Do`. Assistive tech and text
    extraction read `/ActualText` in preference to glyphs.
  - **Tagged-PDF structure tree** (`/StructTreeRoot`, `StructElem /ActualText`/
    `/Alt`) is never walked.
  - **XMP `/Metadata` stream** is not scrubbed: `scrub_pdf_strings` recurses only
    into a stream's *dictionary*, never its raw XML payload
    (`editing.rs:2731-2738`), so a redacted name that is also the XMP
    author/title/`dc:description` survives. (This part alone is Medium — see
    M-7.)
- **Exploitability.** Conditional: requires the sensitive text to exist in one of
  these representations (common for tagged/accessible PDFs and for
  title/author in XMP). When present, the leak is reachable and the content is
  exactly the sensitive text. Rated High because redaction is a security feature
  and these are realistic leak channels; honestly, a plain non-tagged PDF with no
  XMP duplication is not affected by this finding (but is still affected by H-1).
- **Suggested direction.** Parse marked-content property lists during the rewrite
  and strip intersecting `/ActualText`//Alt`; walk `/StructTreeRoot`; strip or
  rewrite the `/Metadata` XMP stream and `/EmbeddedFiles` on redaction.

### M-7 — Redaction metadata scrub misses XMP and embedded-file payloads
- **Severity:** Medium
- **Location:** `editing.rs:2696-2741` (`scrub_pdf_strings`, `Stream { dict, .. }`
  arm at `:2731`); enabled by default (`RedactionOptions::scrub_metadata = true`,
  `editing.rs:179`).
- **Class:** 4.1.
- **Description.** The scrub does a literal `text.replace(secret, "")` over every
  `PdfObject::String` — covering `/Info` and dictionary string values — but it
  never inspects a `Stream`'s raw bytes. Both the XMP `/Metadata` packet and
  embedded-file (`/EmbeddedFiles`) attachment streams are raw payloads, so a
  redacted phrase duplicated there survives. (Folded into H-2; called out
  separately because it is the concrete, isolated code defect.)
- **Suggested direction.** Parse/rewrite the XMP stream (or strip `/Metadata`
  when scrubbing), and enumerate/scrub or strip `/EmbeddedFiles`.

### M-8 — Crypto/signature dependency stack is pre-1.0 on a critical path; `rsa` Marvin reachable
- **Severity:** Medium
- **Location:** `crates/engine/Cargo.toml:15-44`; `Cargo.lock`.
- **Class:** 4.4 — dependency risk.
- **Description.** The security-critical stack is `rsa 0.9.10`, `aes 0.8.4`,
  `cbc 0.1.2`, `cms 0.2.3`, `x509-cert 0.2.5`, `der 0.7.10`, `spki 0.7.3`,
  `const-oid 0.9.6`, `sha2 0.10.9`, `sha1 0.10.6`, `md-5 0.10.6` — all pre-1.0.
  These are RustCrypto mainline (de-facto standard, actively maintained), so 0.x
  reflects RustCrypto's slow path to 1.0 rather than abandonware, but there is no
  SemVer stability guarantee on a signing/encryption path. `rsa 0.9.10` carries
  **RUSTSEC-2023-0071** (Marvin) and is reachable via the signing path (cross-ref
  L-2); it is an explicit, documented `deny.toml` exception. `cms 0.2.3` is a
  comparatively young crate parsing attacker-controlled CMS/X.509.
- **Suggested direction.** Keep `Cargo.lock` committed (it is) and cargo-deny in
  CI; track RustCrypto 1.0 releases and a fixed `rsa`; prioritize the CMS/ASN.1
  parser in the external audit.

### M-9 — Writer: two live generations of one object number collapse to a duplicate definition
- **Severity:** Medium
- **Location:** `writer.rs:1626-1665` (`rewrite_document_objects`, remap keyed by
  number only), via `reader.object_ids()` which does not dedup by number
  (`reader.rs:215-222`).
- **Class:** 4.2 — writer integrity.
- **Description.** A source with two non-free xref entries for the same object
  number at different generations yields two `OutputObject`s sharing one output
  number → two `obj … endobj` bodies with one xref offset → a structurally
  ambiguous (malformed) output. Only reachable via crafted/damaged input fed
  through `optimize`/`repair`/round-trip; normal producers never emit this.
- **Suggested direction.** Dedup `object_ids()` to highest-generation-per-number,
  or key the remap/output on `(number, generation)` end-to-end and detect
  duplicate output numbers before writing.

### M-10 — Writer: unreadable-but-listed object skipped → dangling reference / silent content loss
- **Severity:** Medium
- **Location:** `writer.rs:1646-1669` (and `_with_remap` at `:1689-1711`).
- **Class:** 4.2.
- **Description.** When `get_object` fails for an object listed in
  `object_ids()`, the loop `continue`s but the remap entry remains; referrers get
  rewritten to a new number with no emitted body, and in classic xref the slot
  becomes a free entry — the dangling reference resolves to null/missing
  (content silently lost, not cross-wired). Only triggers on already-damaged
  input; documented/intended for `repair`, less defensible for content-preserving
  `optimize`.
- **Suggested direction.** For `optimize`, treat a fetch failure on a referenced
  object as an error or surface it in the report.

### L-11 — Full-rewrite operations silently invalidate existing signatures
- **Severity:** Low
- **Location:** `structural.rs:145-179` (`optimize`; also `repair`/`linearize`/
  `rotate_pages`) → `rewrite_document_with_mode`.
- **Class:** 4.2 — integrity for signed/archival docs.
- **Description.** These ops produce a fresh xref with renumbered objects,
  invalidating any existing `/ByteRange` signature, with no detection of
  `/Type /Sig` or `/SigFlags` and no warning. (The *signing* path itself is
  correct — the incremental writer preserves prior bytes verbatim,
  `writer.rs:1452`, and `signature.rs` patches `/ByteRange` against the staged
  layout.)
- **Suggested direction.** Detect signed documents at entry to non-incremental
  structural ops and refuse or require explicit opt-in.

### L-12 — Indexed-color lookup length uses unchecked multiply (bounded allocation)
- **Severity:** Low/Info
- **Location:** `images/decoder.rs:614,623` (`decode_indexed`).
- **Class:** 4.5 — decoder.
- **Description.** `(hival + 1) * base_channels` uses unchecked `*` (debug-mode
  panic risk), but only feeds a comparison/warn; the real allocation is bounded
  by the already-materialized `pixels`. Low risk.
- **Suggested direction.** `saturating_mul` for the lookup-length computation.

### Info — `unsafe` inventory: documentation claim verified ACCURATE
- **Severity:** Info (positive confirmation)
- **Class:** 4.3.
- **Finding.** A full-workspace `unsafe` sweep confirms the docs
  (`attack_surface.md:79`, `robustness.md:232`): **all real `unsafe` is in
  `crates/oxide-capi/src/lib.rs`** (the `extern "C"` fn signatures plus
  raw-pointer/`Box`/`CString`/`slice::from_raw_parts` blocks). Crates `engine`,
  `cli`, `server`, `oxide-wasm`, and `oxide-ocr-tesseract` contain **zero** real
  `unsafe` — the only matches elsewhere are the literal word inside comments
  (`images/jpx.rs:10` `#![forbid(unsafe_code)]` in a doc line; `config.rs:228`).
  The FFI `unsafe` is **sound**: null checks, `catch_unwind` on all
  data-returning entry points, correctly paired ownership transfer, and
  documented `# Safety` contracts. **No memory-corruption path from untrusted PDF
  content** — the trust boundary for the C ABI is the C *caller*, not the
  document bytes. (See the one latent fragility in §3 Info: the `*_free`
  functions are outside `catch_unwind`.)

### TIER 3 — verified clean
- **Redaction genuinely rewrites content** (the architecture is correct):
  `Tj`/`TJ`/`'`/`"` show-string bytes are deleted/rewritten, not painted over
  (`editing.rs:1296-1421`); intersecting image XObjects are physically replaced
  with a 1×1 blank `DeviceGray` XObject (`editing.rs:1310-1313,1579-1591`);
  intersecting paths and annotations are dropped (`editing.rs:1266-1278,1602-1622`);
  incremental mode is correctly *rejected* for redaction so the old revision
  bytes are not preserved (`editing.rs:566`). The H-1/H-2 findings are about
  *which* content the geometry selects and which alternate representations are
  reached — not about the removal mechanism.
- **Writer** ObjStm/xref/xref-stream/incremental/merge/dedup/linearize paths
  preserve content or fail loud; reference rewriting maps a missing target to
  `Null`, never cross-wires (`writer.rs:1009-1031`).
- **Decoders** `fonts/type1.rs` (read-then-bounds-check, `MAX_SUBR_DEPTH = 16`),
  `fonts/cmap.rs` (`MAX_CMAP_MAPPINGS = 65_536`), and `images/jpx.rs` (adapter
  sizing from the already-decoded buffer) are bounds-checked/capped.

---

## 5. Clean Areas Reviewed (consolidated)

The following were reviewed and found sound (valuable to record):

- **Crypto primitives:** CSPRNG sourcing, no IV reuse, constant-time password
  verifiers, spec-correct R2–R6 derivation, no padding-oracle signal (§2).
- **Signature soundness:** no path turns a digest/RSA mismatch into `Valid`; cert
  substitution cannot upgrade `Invalid`→`Valid`; fails closed on
  unsupported/missing material; no reachable panic in verification (§2).
- **Memory safety:** `unsafe` confined to the (sound) C ABI; safe-Rust core;
  checked parser arithmetic; bounded xref/object-stream/resolver paths (§3, §4).
- **No SSRF; no active-content execution; no command injection; path-traversal
  blocked** — all confirmed in code, not just docs (§3).
- **Server:** fail-closed constant-time auth, resource caps, worker panic
  isolation, unguessable owner-scoped job IDs, bounded state, restrictive CORS,
  sanitized errors (§3).

---

## 6. Coverage & Residual-Uncertainty Statement (required)

**Audited deeply (line-by-line by the auditor):** `crypto.rs` (2,223 lines),
`signature.rs` (1,726 lines), and the reader's crypto integration
(`reader.rs:445-582`). High-severity claims in TIER 2/3 were re-verified
firsthand against the quoted code (redaction `editing.rs:1142-1421`; decoder
allocations `decoder.rs:228-260,640-663`; CCITT `ccitt.rs:58-76`; CLI signature
reporting `cli/src/main.rs:2010-2092`).

**Audited via focused investigation** (specialist passes, evidence cross-checked
against quoted code): the parser/filters/xref surface
(`parse.rs`/`reader.rs`/`parser.rs`/`object.rs`/`filters.rs`/`document.rs`); the
full server (`crates/server/src/**`); the FFI/WASM/OCR boundaries and
SSRF/active-content/path handling; redaction, writer integrity, the full
`unsafe` inventory, image/font decoders, and dependencies.

**Reviewed lightly or not reached:** the rendering pipeline beyond image decode
(`render/page_renderer.rs` and most of `render/**`, ~15k lines) was assessed only
for the decode-time allocation paths and the documented pixel cap — its
shading/PostScript-function/transparency math was **not** audited for DoS or
correctness; `compliance.rs`, `html.rs`, `analysis/**`, `text/**`, `chunk/**`,
`semantic.rs`, and most font shaping were out of scope. Absence of findings there
is **not** evidence of cleanliness.

**Confirmed-by-code vs suspected-needs-deeper-analysis:**
- *Confirmed by code:* H-1, H-2/M-7, H-3, M-1, M-3, M-4, M-6, M-9, M-10, L-1,
  L-3, L-5, L-6, L-7, L-8, L-9, L-10, L-11, the `unsafe` inventory, and all §5
  clean areas.
- *Suspected / needs dynamic confirmation:* M-5 (JBIG2 OOM) depends on whether
  the `hayro_jbig2` dependency caps region dimensions internally — not verified;
  the precise OOM thresholds for M-3/M-4 depend on the host allocator's behavior
  on a large `with_capacity`; the CMS/ASN.1 parser's DoS resistance on adversarial
  input (§2.7) was reasoned about but **not** fuzzed in this pass.

**Where an AI read-only review is inherently weak — needs the paid human
audit / dynamic analysis:**
- **Cryptographic math correctness** of the R2–R6 derivation and CMS handling
  beyond structure (subtle spec-conformance and interop edge cases).
- **Timing side-channels** — `subtle`/`ct_eq` usage was confirmed structurally,
  but constant-time behavior cannot be *proven* by reading source; the `rsa`
  Marvin exposure (L-2/M-8) needs an expert's judgment on the deployment.
- **Subtle protocol-level signature attacks** (shadow attacks, polyglot/incremental
  re-save variants, CMS canonicalization edge cases) — M-2 is the structural
  symptom, but exhaustive coverage of this class requires a signature-security
  specialist and a malicious-PDF corpus.
- **Decoder DoS/correctness** in the third-party `hayro-*` codecs (M-5, DEP-3).

> **This audit is not exhaustive. Absence of findings ≠ absence of
> vulnerabilities.** It covers the known high-risk classes against this
> codebase's actual surfaces and complements, but does not replace, a paid
> third-party human audit — most importantly for the ~4,000 lines of
> crypto/signature code.

---

## 7. Prioritized Remediation List (punch list — fixing is a separate task)

Ordered by severity × exploitability. Each item is independent; fix per-finding.

| # | Finding | Sev | Why it's high on the list |
| --- | --- | --- | --- |
| 1 | **H-1** Redaction under-removes text (fake glyph metrics) — `editing.rs:1142,1149` | High | A security feature silently leaking the exact data it's meant to remove, on most real fonts. Fail-closed + real font widths. |
| 2 | **H-4/H-5/H-6** Image-decoder OOM from unbounded `width×height` — `decoder.rs:234`, `ccitt.rs:66`, `jbig2.rs:28` | High | Trivial untrusted-PDF OOM; reachable from server endpoints. Add a decode-layer pixel cap + `checked_mul`. |
| 3 | **H-2 / M-7** Redaction skips alternate text reps (inline `/ActualText`, struct tree, XMP, attachments) | High/Med | Compounds H-1; reachable on tagged PDFs / XMP-duplicated text. |
| 4 | **H-3** `Valid` ≠ trusted (no chain/validity/revocation gating) — `signature.rs:1005,1124` | High | Easy programmatic misread → accepting attacker-signed docs. Add trust validation and/or type-level clarity. |
| 5 | **M-1** Unbounded page-tree recursion → abort — `document.rs:241` | Med | Stack-overflow process abort from a crafted file; add a depth cap like elsewhere. |
| 6 | **M-2** Incomplete `/ByteRange` coverage validation — `signature.rs:1620` | Med | Partial-coverage/shadow rigor gap in the highest-stakes logic. |
| 7 | **M-6** `expected_len` unchecked multiply (32-bit wrap / forced resize) — `decoder.rs:648` | Med | Wrong-size buffers on 32-bit/wasm; `checked_mul`. |
| 8 | **M-9 / M-10** Writer gen-collision duplicate / dangling-on-unreadable — `writer.rs:1626,1646` | Med | Integrity bugs (malformed output / silent content loss) on crafted/damaged input. |
| 9 | **M-8** Pre-1.0 crypto stack + reachable `rsa` Marvin — `Cargo.toml`, `Cargo.lock` | Med | Prioritize CMS/ASN.1 + `rsa` in the external audit; track 1.0/fixed releases. |
| 10 | **L-1** SHA-1 signatures blessed as `Valid` — `signature.rs:1189` | Low | Flag/deprioritize weak digests. |
| 11 | **L-5** LZW/RunLength outputs uncapped — `filters.rs` | Low | Add a cumulative filter-chain output ceiling. |
| 12 | **L-6** Sync server ingest outside panic-isolation/deadline; pin `panic="unwind"` | Low | Becomes High if `panic="abort"` is ever set. |
| 13 | **L-3** V5 `/Perms` integrity not enforced; `/P` not enforced — `crypto.rs:963`, `reader.rs:559` | Low | Validate `/Perms` against `/P`; document that `/P` is advisory. |
| 14 | **L-4** Key material not zeroized (`zeroize` already available) — `crypto.rs` | Low | Wrap keys/intermediates/passwords in `Zeroizing`. |
| 15 | **L-7/L-8/L-9/L-10/L-11/L-12** + C-ABI `*_free` `catch_unwind` (Info) | Low/Info | Defense-in-depth hardening; address opportunistically. |

---

*End of findings. This document is the only file created by this audit; no
existing source was modified. The audit was read-only.*
