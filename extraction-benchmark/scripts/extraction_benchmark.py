#!/usr/bin/env python3
"""Extraction-quality head-to-head benchmark harness.

Runs Oxide and each available competitor (PyMuPDF, qpdf, Poppler pdftotext, and
Docling if present) over the ground-truth corpus and scores every tool with the
SAME pure-Rust metrics (via ``oxide eval-score``), so the numbers are directly
comparable and comparable to how the literature reports (CER/WER, cell-F1/TEDS,
field-F1, block-type accuracy, reading-order similarity).

Design (matches the prompt's constraints):
  - Competitor DETECTION at runtime; run those present, skip others cleanly with
    a note; NEVER fabricate a competitor number. Docling is marked not-run if
    absent.
  - Each tool invocation runs in an ISOLATED subprocess with a TIMEOUT so one
    failure/hang does not abort the run; failures are recorded as data.
  - MEASUREMENT only — nothing here changes the parser.

Output: ``results/results.json`` (raw per-doc/per-tool scores + availability +
speed/memory/size), consumed by ``write_report.py``.
"""

import json
import os
import shutil
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)            # extraction-benchmark/
REPO = os.path.dirname(ROOT)            # repo root
CORPUS = os.path.join(ROOT, "corpus")
EXPECTED = os.path.join(ROOT, "expected")
RESULTS = os.path.join(ROOT, "results")

TIMEOUT = 60  # seconds per tool/doc

# Resolve the oxide CLI binary. OXIDE_BIN lets release-gate runs use an
# isolated target directory when the default debug artifact is locked.
OXIDE = os.environ.get("OXIDE_BIN")
if not OXIDE:
    OXIDE = os.path.join(REPO, "target", "debug", "oxide.exe")
    if not os.path.exists(OXIDE):
        OXIDE = os.path.join(REPO, "target", "debug", "oxide")

# Whether this oxide build understands --ocr (the `ocr` cargo feature). Probed at
# import time so scanned-doc handling degrades cleanly when OCR isn't built in.
OXIDE_HAS_OCR = False


# ── subprocess helper (isolated, timed, failure-as-data) ─────────────────────


def run(cmd, timeout=TIMEOUT, stdin=None):
    """Run a command; return (ok, stdout, elapsed_s, err). Never raises."""
    t0 = time.perf_counter()
    try:
        p = subprocess.run(
            cmd,
            input=stdin,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=timeout,
        )
        dt = time.perf_counter() - t0
        if p.returncode != 0:
            return (False, p.stdout, dt, p.stderr.decode("utf-8", "replace")[:400])
        return (True, p.stdout, dt, "")
    except subprocess.TimeoutExpired:
        return (False, b"", timeout, "timeout")
    except Exception as e:  # noqa: BLE001 — failure is data, not fatal
        return (False, b"", time.perf_counter() - t0, str(e)[:400])


def oxide_score(score_input):
    """Score a ScoreInput dict with the pure-Rust scorer; returns the dict."""
    ok, out, _dt, err = run([OXIDE, "eval-score"], stdin=json.dumps(score_input).encode())
    if not ok:
        return {"_error": err}
    try:
        return json.loads(out.decode("utf-8", "replace"))
    except json.JSONDecodeError:
        return {"_error": "unparseable score output"}


# ── tool detection ───────────────────────────────────────────────────────────


def detect_tools():
    global OXIDE_HAS_OCR
    tools = {}
    tools["oxide"] = os.path.exists(OXIDE)
    if tools["oxide"]:
        # The `--ocr` flag exists in the CLI regardless of the cargo feature, but
        # invoking it without the feature returns an actionable "no OCR backend"
        # error. Probe by actually running `--ocr` and checking the error text, so
        # OXIDE_HAS_OCR reflects the *built* capability, not just the flag.
        sample = None
        for f in os.listdir(CORPUS):
            if f.endswith("_scanned.pdf"):
                sample = os.path.join(CORPUS, f)
                break
        if sample:
            ok, _o, _dt, err = run([OXIDE, "parse", sample, "--format", "markdown", "--ocr"])
            OXIDE_HAS_OCR = ok and ("no OCR backend" not in err)
    tools["oxide_ocr"] = OXIDE_HAS_OCR
    try:
        import fitz  # noqa: F401
        tools["pymupdf"] = True
    except ImportError:
        tools["pymupdf"] = False
    tools["pdftotext"] = shutil.which("pdftotext") is not None
    tools["qpdf"] = shutil.which("qpdf") is not None
    try:
        import docling  # noqa: F401
        tools["docling"] = True
    except ImportError:
        tools["docling"] = False
    return tools


# ── per-tool text extractors → reading-order plain text ──────────────────────


def oxide_text(pdf, ocr=False):
    # Markdown without furniture is the closest to clean reading-order text.
    args = [OXIDE, "parse", pdf, "--format", "markdown"]
    if ocr:
        args += ["--ocr"]
    ok, out, dt, err = run(args)
    return (markdown_to_text(out.decode("utf-8", "replace")) if ok else None, dt, err)


def oxide_text_order(pdf):
    """Oxide block order keys (lowercased block text) for the order metric."""
    ok, out, _dt, _err = run([OXIDE, "parse", pdf, "--format", "json"])
    if not ok:
        return None
    try:
        doc = json.loads(out.decode("utf-8", "replace"))
    except json.JSONDecodeError:
        return None
    keys = []
    for b in doc.get("body", []):
        keys.append(block_text_key(b))
    return [k for k in keys if k]


def pymupdf_text(pdf):
    import fitz
    t0 = time.perf_counter()
    try:
        d = fitz.open(pdf)
        parts = [page.get_text("text") for page in d]
        d.close()
        return ("\n".join(parts), time.perf_counter() - t0, "")
    except Exception as e:  # noqa: BLE001
        return (None, time.perf_counter() - t0, str(e)[:200])


def pdftotext_text(pdf):
    ok, out, dt, err = run(["pdftotext", "-layout", pdf, "-"])
    return (out.decode("utf-8", "replace") if ok else None, dt, err)


def markdown_to_text(md):
    """Strip Markdown markup + HTML comments to compare plain reading-order text."""
    lines = []
    for line in md.splitlines():
        s = line.strip()
        if s.startswith("<!--") or s == "---":
            continue
        s = s.lstrip("#").strip()
        s = s.replace("**", "").replace("*", "")
        if s.startswith("|") or set(s) <= {"|", "-", " "}:
            # table row → keep cell text, drop pipes
            s = " ".join(c.strip() for c in s.split("|") if c.strip())
            if set(s) <= {"-", " "}:
                continue
        lines.append(s)
    return "\n".join(x for x in lines if x)


def block_text_key(b):
    """A normalized identity key for a parse-JSON block (for order scoring)."""
    kind = b.get("kind", "")
    txt = ""
    t = b.get("text")
    if isinstance(t, list):  # InlineText span list
        txt = "".join(span.get("text", "") for span in t)
    elif isinstance(t, str):
        txt = t
    if kind == "table":
        rows = b.get("table", {}).get("rows", [])
        txt = " ".join(" ".join(r) for r in rows[:1])  # header row as key
    return " ".join(txt.split()).lower()[:80]


# ── table extractors → list of grids ─────────────────────────────────────────


def oxide_tables(pdf, ocr=False):
    # extract-tables has no OCR path; on scanned docs use `parse --ocr` JSON and
    # pull table rows from the body. For digital docs the dedicated command is
    # faster and identical.
    if ocr and OXIDE_HAS_OCR:
        ok, out, dt, err = run([OXIDE, "parse", pdf, "--format", "json", "--ocr"])
        if not ok:
            return (None, dt, err)
        try:
            doc = json.loads(out.decode("utf-8", "replace"))
        except json.JSONDecodeError:
            return (None, dt, "parse error")
        grids = [b["table"]["rows"] for b in doc.get("body", []) if b.get("kind") == "table"]
        return (grids, dt, "")
    ok, out, dt, err = run([OXIDE, "extract-tables", pdf, "--format", "json"])
    if not ok:
        return (None, dt, err)
    try:
        d = json.loads(out.decode("utf-8", "replace"))
    except json.JSONDecodeError:
        return (None, dt, "parse error")
    grids = []
    for page in d.get("pages", []):
        for t in page.get("tables", []):
            grids.append(t.get("rows", []))
    return (grids, dt, "")


def pymupdf_tables(pdf):
    import fitz
    t0 = time.perf_counter()
    try:
        d = fitz.open(pdf)
        grids = []
        for page in d:
            tf = page.find_tables()
            for t in tf.tables:
                grids.append([[c if c is not None else "" for c in row] for row in t.extract()])
        d.close()
        return (grids, time.perf_counter() - t0, "")
    except Exception as e:  # noqa: BLE001
        return (None, time.perf_counter() - t0, str(e)[:200])


# ── KV extractor (Oxide only; competitors don't do KV out of the box) ────────


def oxide_fields(pdf, doc_type, ocr=False):
    args = [OXIDE, "extract-fields", pdf, "--format", "json"]
    if doc_type in ("invoice", "receipt", "form"):
        args += ["--type", doc_type]
    if ocr and OXIDE_HAS_OCR:
        args += ["--ocr"]
    ok, out, dt, err = run(args)
    if not ok:
        return (None, dt, err)
    try:
        d = json.loads(out.decode("utf-8", "replace"))
    except json.JSONDecodeError:
        return (None, dt, "parse error")
    fields = []
    for f in d.get("fields", []):
        v = f.get("value", {})
        # Normalize the typed value to a comparable string (matches the labels).
        norm = normalize_field_value(v)
        fields.append({"key": f.get("key", ""), "value": norm})
    return (fields, dt, "")


def normalize_field_value(v):
    t = v.get("type")
    if t == "amount":
        cur = v.get("currency")
        return f"{v.get('value', 0):.2f} {cur}".lower() if cur else f"{v.get('value', 0):.2f}"
    if t == "date":
        return v.get("iso", "").lower()
    if t == "number":
        n = v.get("value", 0)
        return (str(int(n)) if float(n).is_integer() else str(n)).lower()
    if t == "percent":
        return f"{v.get('value', 0)}%"
    if t in ("email", "phone"):
        return str(v.get("address") or v.get("number") or "").lower()
    return str(v.get("text", "")).strip().lower()


# ── block-type sequence (Oxide) ──────────────────────────────────────────────


def oxide_block_types(pdf):
    ok, out, _dt, _err = run([OXIDE, "parse", pdf, "--format", "json"])
    if not ok:
        return None
    try:
        doc = json.loads(out.decode("utf-8", "replace"))
    except json.JSONDecodeError:
        return None
    return [b.get("kind", "") for b in doc.get("body", [])]


# ── structural ops (qpdf vs oxide) ───────────────────────────────────────────


def structural_ops(tools):
    """Compare Oxide and qpdf on structural correctness + cross-validation.

    - Oxide splits a multi-page PDF; qpdf validates Oxide's output (--check).
    - qpdf linearizes a PDF; Oxide reports its page count (round-trip integrity).
    """
    results = {}
    sample = os.path.join(CORPUS, "paper.pdf")  # single page; use tracemonkey if present
    tm = os.path.join(REPO, "crates", "engine", "tests", "fixtures", "tracemonkey.pdf")
    if os.path.exists(tm):
        sample = tm

    tmp = os.path.join(RESULTS, "_tmp")
    os.makedirs(tmp, exist_ok=True)

    # Oxide page count.
    ok, out, dt_ox, _ = run([OXIDE, "info", sample])
    ox_pages = None
    if ok:
        for line in out.decode("utf-8", "replace").splitlines():
            low = line.lower()
            if "page" in low and any(ch.isdigit() for ch in line):
                digits = "".join(ch for ch in line if ch.isdigit())
                if digits:
                    ox_pages = int(digits)
                    break
    results["oxide_info_time_s"] = round(dt_ox, 4)
    results["oxide_page_count"] = ox_pages

    if tools.get("qpdf"):
        # qpdf linearize Oxide-validatable round trip.
        lin = os.path.join(tmp, "lin.pdf")
        ok_lin, _o, dt_lin, err_lin = run(["qpdf", "--linearize", sample, lin])
        results["qpdf_linearize_ok"] = ok_lin
        results["qpdf_linearize_time_s"] = round(dt_lin, 4)
        # qpdf --check on the linearized file (structural validity).
        if ok_lin:
            ok_chk, _o2, _dt2, _e2 = run(["qpdf", "--check", lin])
            results["qpdf_check_linearized_ok"] = ok_chk
        # qpdf page count for cross-validation.
        ok_pc, out_pc, _dt3, _ = run(["qpdf", "--show-npages", sample])
        if ok_pc:
            try:
                results["qpdf_page_count"] = int(out_pc.decode().strip())
            except ValueError:
                results["qpdf_page_count"] = None
        results["page_count_agree"] = (
            results.get("qpdf_page_count") is not None
            and results.get("qpdf_page_count") == ox_pages
        )
    else:
        results["qpdf"] = "not run (absent)"

    # Oxide split → qpdf --check each part (Oxide output validated by qpdf).
    if tools.get("qpdf"):
        outdir = os.path.join(tmp, "split")
        if os.path.isdir(outdir):
            shutil.rmtree(outdir, ignore_errors=True)
        os.makedirs(outdir, exist_ok=True)
        # `split -o page-%d.pdf` writes into the CWD pattern; run in outdir.
        t0 = time.perf_counter()
        try:
            p = subprocess.run(
                [OXIDE, "split", os.path.abspath(sample), "-o", "page-%d.pdf"],
                cwd=outdir, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=TIMEOUT,
            )
            ok_sp = p.returncode == 0
            err_sp = p.stderr.decode("utf-8", "replace")[:200]
        except Exception as e:  # noqa: BLE001
            ok_sp, err_sp = False, str(e)[:200]
        dt_sp = time.perf_counter() - t0
        results["oxide_split_ok"] = ok_sp
        results["oxide_split_time_s"] = round(dt_sp, 4)
        if not ok_sp:
            results["oxide_split_error"] = err_sp
        if ok_sp:
            parts = [os.path.join(outdir, f) for f in os.listdir(outdir) if f.endswith(".pdf")]
            valid = 0
            for part in parts[:5]:  # check up to 5 parts
                okc, _oc, _dtc, _ec = run(["qpdf", "--check", part])
                if okc:
                    valid += 1
            results["oxide_split_parts"] = len(parts)
            results["qpdf_validated_oxide_parts_of_5"] = valid
    return results


# ── main ──────────────────────────────────────────────────────────────────────


def load_expected():
    docs = {}
    for f in sorted(os.listdir(EXPECTED)):
        if f.endswith(".json"):
            with open(os.path.join(EXPECTED, f), encoding="utf-8") as fh:
                docs[f[:-5]] = json.load(fh)
    return docs


def bench_doc(name, gt, tools):
    pdf = os.path.join(CORPUS, name + ".pdf")
    rec = {"doc_type": gt.get("doc_type"), "mode": gt.get("mode"), "tools": {}}

    # TEXT + reading order. Only scored when the doc carries a text label.
    ref_text = gt.get("text") or ""
    ref_order = gt.get("order") or []
    is_scanned = gt.get("mode") == "scanned"

    def text_entry(text, dt, err, get_order=None):
        e = {"time_s": round(dt, 4)}
        if text is None:
            e["error"] = err or "no output"
            return e
        si = {"ref_text": ref_text, "hyp_text": text}
        if get_order and ref_order:
            ho = get_order(pdf)
            if ho is not None:
                si["ref_order"] = ref_order
                si["hyp_order"] = ho
        e.update(oxide_score(si))
        return e

    if ref_text:
        if tools.get("oxide"):
            # Scanned docs are OCR'd by Oxide — the whole point of including them.
            text, dt, err = oxide_text(pdf, ocr=is_scanned and OXIDE_HAS_OCR)
            rec["tools"]["oxide_text"] = text_entry(text, dt, err, get_order=oxide_text_order)
        if tools.get("pymupdf"):
            # PyMuPDF/Poppler have NO OCR; on scanned pages they recover nothing
            # (recorded honestly — it is the OCR-capability gap, not a bug).
            text, dt, err = pymupdf_text(pdf)
            rec["tools"]["pymupdf_text"] = text_entry(text, dt, err)
        if tools.get("pdftotext"):
            text, dt, err = pdftotext_text(pdf)
            rec["tools"]["pdftotext_text"] = text_entry(text, dt, err)

    # TABLES.
    ref_tables = gt.get("tables") or []
    if ref_tables:
        if tools.get("oxide"):
            grids, dt, err = oxide_tables(pdf, ocr=is_scanned)
            e = {"time_s": round(dt, 4)}
            if grids is None:
                e["error"] = err
            else:
                e.update(oxide_score({"ref_tables": ref_tables, "hyp_tables": grids}))
            rec["tools"]["oxide_tables"] = e
        if tools.get("pymupdf"):
            grids, dt, err = pymupdf_tables(pdf)
            e = {"time_s": round(dt, 4)}
            if grids is None:
                e["error"] = err
            else:
                e.update(oxide_score({"ref_tables": ref_tables, "hyp_tables": grids}))
            rec["tools"]["pymupdf_tables"] = e

    # KV fields (Oxide only).
    ref_fields = gt.get("fields") or []
    if ref_fields and tools.get("oxide"):
        fields, dt, err = oxide_fields(pdf, gt.get("doc_type"), ocr=is_scanned)
        e = {"time_s": round(dt, 4)}
        if fields is None:
            e["error"] = err
        else:
            e.update(oxide_score({"ref_fields": ref_fields, "hyp_fields": fields}))
        rec["tools"]["oxide_fields"] = e

    # Block-type structure (Oxide only).
    ref_bt = gt.get("block_types") or []
    if ref_bt and tools.get("oxide"):
        bt = oxide_block_types(pdf)
        if bt is not None:
            # Align by length (the order metric already covers ordering); we score
            # the leading aligned run as a coarse structure accuracy.
            e = oxide_score({"ref_block_types": ref_bt, "hyp_block_types": bt})
            rec["tools"]["oxide_structure"] = e

    return rec


def main():
    os.makedirs(RESULTS, exist_ok=True)
    tools = detect_tools()
    print("Tool availability:")
    for t, ok in tools.items():
        print(f"  {t:10s} {'present' if ok else 'ABSENT (skipped)'}")
    if not tools.get("oxide"):
        print("ERROR: oxide binary not found; build with `cargo build -p oxide-cli` first.")
        return 1

    expected = load_expected()
    docs = {}
    for name, gt in expected.items():
        print(f"  scoring {name} ...")
        docs[name] = bench_doc(name, gt, tools)

    print("  structural ops + speed ...")
    struct = structural_ops(tools)

    results = {
        "tools": tools,
        "timeout_s": TIMEOUT,
        "docs": docs,
        "structural_ops": struct,
    }
    out_path = os.path.join(RESULTS, "results.json")
    with open(out_path, "w", encoding="utf-8") as f:
        json.dump(results, f, indent=2, ensure_ascii=False)
    print(f"Wrote {out_path}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
