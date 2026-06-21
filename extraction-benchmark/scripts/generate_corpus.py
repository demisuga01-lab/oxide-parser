#!/usr/bin/env python3
"""Generate the extraction-benchmark ground-truth corpus.

Produces, under ``extraction-benchmark/corpus/`` (PDFs) and ``expected/`` (the
ground-truth JSON labels), a small but well-labeled set of documents. Because we
author the PDF *and* its labels together, the ground truth is exact by
construction — the most reliable kind of extraction label.

Doc types (digital-born):
  - report_multicol : a two-column page (reading-order stress test)
  - paper           : a single-column "paper" with headings + paragraphs
  - tables          : a page dominated by a ruled-ish data table
  - invoice         : labeled header fields + line-item table + totals (KV)
  - receipt         : a small receipt (KV)
  - figure          : a figure with a caption

Scanned variants (image-only, to score the OCR path) are produced for a subset
by rasterizing the digital PDF and re-wrapping the pixels as an image-only PDF.

Dependencies: reportlab (pure-Python PDF writer — a *writer*, not an extractor,
so it does not bias the comparison) and PyMuPDF (used only as a rasterizer for
the scanned variants — a utility role, not under test there). Both are dev
tooling; Oxide's own side stays pure-Rust.

Sources/licenses: all documents are SYNTHETIC and self-authored for this
benchmark (no third-party corpus is redistributed), so the corpus is freely
usable. Real public datasets (DocLayNet, FUNSD, SROIE, PubLayNet) can be dropped
into corpus/ with matching expected/*.json later using the same label schema.
"""

import json
import os
import sys

from reportlab.pdfgen import canvas
from reportlab.lib.pagesizes import letter

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
CORPUS = os.path.join(ROOT, "corpus")
EXPECTED = os.path.join(ROOT, "expected")
PAGE_W, PAGE_H = letter  # 612 x 792


# ── label schema ─────────────────────────────────────────────────────────────
# expected/<name>.json:
# {
#   "doc_type": "...",            # for the KV profile + per-type breakdown
#   "path": "1",                  # scanned / digital tag
#   "text": "reading-order plain text",
#   "order": ["block-key", ...],  # block identity keys in reading order
#   "tables": [ [[cell,...],...], ... ],
#   "fields": [ {"key":..., "value": normalized}, ... ],
#   "block_types": ["heading","paragraph",...]
# }


def gt(doc_type, mode, text, order, tables, fields, block_types):
    return {
        "doc_type": doc_type,
        "mode": mode,
        "text": text,
        "order": order,
        "tables": tables,
        "fields": fields,
        "block_types": block_types,
    }


def write(name, gtruth):
    with open(os.path.join(EXPECTED, name + ".json"), "w", encoding="utf-8") as f:
        json.dump(gtruth, f, indent=2, ensure_ascii=False)


# ── generators ───────────────────────────────────────────────────────────────


def gen_paper():
    name = "paper"
    c = canvas.Canvas(os.path.join(CORPUS, name + ".pdf"), pagesize=letter)
    lines = []

    def heading(y, size, s):
        c.setFont("Helvetica-Bold", size)
        c.drawString(72, y, s)

    def body(y, s):
        c.setFont("Helvetica", 11)
        c.drawString(72, y, s)

    heading(740, 18, "A Study of Document Parsing")
    body(710, "This paper introduces a structure-aware approach to extraction.")
    body(694, "It recovers reading order, tables, and fields from PDF documents.")
    heading(660, 14, "1. Introduction")
    body(636, "Documents encode structure that naive text dumps discard entirely.")
    body(620, "We preserve headings, paragraphs, and lists in reading order.")
    heading(586, 14, "2. Method")
    body(562, "A geometric precedence graph orders blocks across columns.")
    body(546, "Tables are detected from ruling lines and text alignment.")
    c.showPage()
    c.save()

    text = "\n".join([
        "A Study of Document Parsing",
        "This paper introduces a structure-aware approach to extraction.",
        "It recovers reading order, tables, and fields from PDF documents.",
        "1. Introduction",
        "Documents encode structure that naive text dumps discard entirely.",
        "We preserve headings, paragraphs, and lists in reading order.",
        "2. Method",
        "A geometric precedence graph orders blocks across columns.",
        "Tables are detected from ruling lines and text alignment.",
    ])
    order = [
        "a study of document parsing",
        "this paper introduces a structure-aware approach to extraction.",
        "it recovers reading order, tables, and fields from pdf documents.",
        "1. introduction",
        "documents encode structure that naive text dumps discard entirely.",
        "we preserve headings, paragraphs, and lists in reading order.",
        "2. method",
        "a geometric precedence graph orders blocks across columns.",
        "tables are detected from ruling lines and text alignment.",
    ]
    block_types = ["title", "paragraph", "paragraph", "heading", "paragraph",
                   "paragraph", "heading", "paragraph", "paragraph"]
    write(name, gt("generic", "digital", text, order, [], [], block_types))


def gen_report_multicol():
    """Two-column page: the left column reads fully before the right. A naive
    top-to-bottom dump interleaves them; the order metric rewards getting it
    right."""
    name = "report_multicol"
    c = canvas.Canvas(os.path.join(CORPUS, name + ".pdf"), pagesize=letter)
    c.setFont("Helvetica-Bold", 16)
    c.drawString(72, 750, "Quarterly Report")
    c.setFont("Helvetica", 11)
    # Left column (x=72), then right column (x=320). Same y rows.
    left = [
        "Left column line one of the report.",
        "Left column line two continues here.",
        "Left column line three wraps the idea.",
        "Left column line four concludes left.",
    ]
    right = [
        "Right column line one starts here.",
        "Right column line two adds detail.",
        "Right column line three explains more.",
        "Right column line four ends the right.",
    ]
    y = 720
    for i in range(4):
        c.drawString(72, y - i * 16, left[i])
        c.drawString(320, y - i * 16, right[i])
    c.showPage()
    c.save()

    # Correct reading order: title, then all of left, then all of right.
    ordered = ["Quarterly Report"] + left + right
    text = "\n".join(ordered)
    order = [s.lower() for s in ordered]
    block_types = ["title"] + ["paragraph"] * 8
    write(name, gt("generic", "digital", text, order, [], [], block_types))


def gen_tables():
    name = "tables"
    c = canvas.Canvas(os.path.join(CORPUS, name + ".pdf"), pagesize=letter)
    c.setFont("Helvetica-Bold", 14)
    c.drawString(72, 740, "Measurements")
    # A ruled 3x3 table.
    rows = [["City", "Temp", "Humidity"],
            ["Paris", "21", "55%"],
            ["Tokyo", "28", "70%"]]
    x0, y0 = 72, 700
    col_w = [120, 80, 100]
    row_h = 22
    c.setFont("Helvetica", 11)
    # Draw grid lines.
    n_rows, n_cols = len(rows), len(rows[0])
    total_w = sum(col_w)
    for r in range(n_rows + 1):
        c.line(x0, y0 - r * row_h, x0 + total_w, y0 - r * row_h)
    cx = x0
    for ci in range(n_cols + 1):
        c.line(cx, y0, cx, y0 - n_rows * row_h)
        if ci < n_cols:
            cx += col_w[ci]
    # Draw cell text.
    for r, row in enumerate(rows):
        cx = x0 + 5
        for ci, cell in enumerate(row):
            c.drawString(cx, y0 - r * row_h - 15, cell)
            cx += col_w[ci]
    c.showPage()
    c.save()

    text = "Measurements\n" + "\n".join(" ".join(r) for r in rows)
    order = ["measurements"] + [" ".join(r).lower() for r in rows]
    block_types = ["heading", "table"]
    write(name, gt("generic", "digital", text, order, [rows], [], block_types))


def gen_invoice():
    name = "invoice"
    c = canvas.Canvas(os.path.join(CORPUS, name + ".pdf"), pagesize=letter)
    c.setFont("Helvetica-Bold", 20)
    c.drawString(72, 740, "Acme Supplies Inc")
    c.setFont("Helvetica-Bold", 16)
    c.drawString(72, 715, "INVOICE")
    c.setFont("Helvetica", 11)
    pairs = [
        ("Invoice Number:", "INV-2024-0042"),
        ("Invoice Date:", "Jan 15, 2024"),
        ("Due Date:", "2024-02-15"),
        ("Bill To:", "Globex Corporation"),
    ]
    y = 685
    for k, v in pairs:
        c.drawString(72, y, k)
        c.drawString(200, y, v)
        y -= 16
    # Line-item table.
    rows = [["Description", "Qty", "Unit Price", "Amount"],
            ["Widget assembly", "10", "$25.00", "$250.00"],
            ["Premium gizmo", "2", "$100.00", "$200.00"]]
    cols = [72, 320, 400, 490]
    yt = 600
    for row in rows:
        for i, cell in enumerate(row):
            c.drawString(cols[i], yt, cell)
        yt -= 18
    # Totals.
    totals = [("Subtotal:", "$450.00"), ("Tax:", "$36.00"), ("Total:", "$486.00")]
    y = 520
    for k, v in totals:
        c.drawString(360, y, k)
        c.drawString(490, y, v)
        y -= 16
    c.showPage()
    c.save()

    fields = [
        {"key": "invoice_number", "value": "inv-2024-0042"},
        {"key": "invoice_date", "value": "2024-01-15"},
        {"key": "due_date", "value": "2024-02-15"},
        {"key": "bill_to", "value": "globex corporation"},
        {"key": "subtotal", "value": "450.00 usd"},
        {"key": "tax", "value": "36.00 usd"},
        {"key": "total", "value": "486.00 usd"},
    ]
    # Invoice is a KV/table-focused doc; free-text reading order is not the
    # capability under test here, so leave `text` empty (the harness skips text
    # scoring) and score it on fields + the line-item table instead.
    write(name, gt("invoice", "digital", "", [], [rows], fields, []))


def gen_receipt():
    name = "receipt"
    c = canvas.Canvas(os.path.join(CORPUS, name + ".pdf"), pagesize=(300, 500))
    c.setFont("Helvetica-Bold", 14)
    c.drawString(40, 460, "Joe's Coffee Shop")
    c.setFont("Helvetica", 10)
    c.drawString(40, 440, "RECEIPT")
    c.drawString(40, 420, "Date: 03/22/2024")
    pairs = [("Subtotal:", "$8.50"), ("Tax:", "$0.68"), ("Total:", "$9.18")]
    y = 380
    for k, v in pairs:
        c.drawString(40, y, k)
        c.drawString(200, y, v)
        y -= 14
    c.drawString(40, 330, "Payment: VISA ****1234")
    c.showPage()
    c.save()

    fields = [
        {"key": "merchant", "value": "joe's coffee shop"},
        {"key": "date", "value": "2024-03-22"},
        {"key": "subtotal", "value": "8.50 usd"},
        {"key": "tax", "value": "0.68 usd"},
        {"key": "total", "value": "9.18 usd"},
    ]
    # Receipt is KV-focused; score on fields, not free-text reading order.
    write(name, gt("receipt", "digital", "", [], [], fields, []))


def gen_figure():
    name = "figure"
    c = canvas.Canvas(os.path.join(CORPUS, name + ".pdf"), pagesize=letter)
    c.setFont("Helvetica-Bold", 16)
    c.drawString(72, 740, "Results")
    # A drawn "figure" (a filled rectangle box).
    c.rect(72, 560, 300, 150, fill=0)
    c.line(72, 560, 372, 710)
    c.setFont("Helvetica-Oblique", 10)
    c.drawString(72, 545, "Figure 1. A sample chart of the results.")
    c.setFont("Helvetica", 11)
    c.drawString(72, 510, "The figure above summarizes the experimental outcome.")
    c.showPage()
    c.save()

    text = "Results\nFigure 1. A sample chart of the results.\nThe figure above summarizes the experimental outcome."
    order = ["results", "figure 1. a sample chart of the results.",
             "the figure above summarizes the experimental outcome."]
    block_types = ["heading", "figure", "caption", "paragraph"]
    write(name, gt("generic", "digital", text, order, [], [], block_types))


# ── scanned variants ─────────────────────────────────────────────────────────


def make_scanned(src_name, dst_name, dpi=200):
    """Rasterize a digital PDF and re-wrap the pixels as an image-only PDF, so the
    classifier routes it to OCR. Uses PyMuPDF purely as a rasterizer (utility),
    copying the source's ground-truth labels (mode→scanned).

    The page image is embedded **PNG-compressed** at a modest DPI so the corpus
    stays small enough to commit (an uncompressed pixmap is ~10 MB/page); 150 DPI
    grayscale PNG is still comfortably legible for Tesseract.
    """
    try:
        import fitz  # PyMuPDF
    except ImportError:
        print(f"  [skip scanned {dst_name}: PyMuPDF not available]")
        return
    src = fitz.open(os.path.join(CORPUS, src_name + ".pdf"))
    out = fitz.open()
    for page in src:
        # Color pixmap → PNG bytes → embed. PNG keeps glyph contrast (grayscale
        # re-encode noticeably degraded OCR), while PNG compression keeps the file
        # ~2 orders of magnitude smaller than an embedded raw pixmap.
        pix = page.get_pixmap(dpi=dpi)
        png = pix.tobytes("png")
        rect = page.rect
        newp = out.new_page(width=rect.width, height=rect.height)
        newp.insert_image(rect, stream=png)
    out.save(os.path.join(CORPUS, dst_name + ".pdf"), deflate=True, garbage=4)
    out.close()
    src.close()
    # Copy labels, marking mode=scanned.
    with open(os.path.join(EXPECTED, src_name + ".json"), encoding="utf-8") as f:
        g = json.load(f)
    g["mode"] = "scanned"
    write(dst_name, g)


def main():
    os.makedirs(CORPUS, exist_ok=True)
    os.makedirs(EXPECTED, exist_ok=True)
    gen_paper()
    gen_report_multicol()
    gen_tables()
    gen_invoice()
    gen_receipt()
    gen_figure()
    # Scanned variants of a representative subset (text, table, invoice).
    make_scanned("paper", "paper_scanned")
    make_scanned("tables", "tables_scanned")
    make_scanned("invoice", "invoice_scanned")

    docs = sorted(f[:-4] for f in os.listdir(CORPUS) if f.endswith(".pdf"))
    print(f"Generated {len(docs)} documents: {', '.join(docs)}")


if __name__ == "__main__":
    sys.exit(main())
