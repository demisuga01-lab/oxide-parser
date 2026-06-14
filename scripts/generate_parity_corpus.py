#!/usr/bin/env python3
"""Build the Poppler parity corpus and manifest."""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import sys
import urllib.error
import urllib.request
import zlib
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


REPO_ROOT = Path(__file__).resolve().parents[1]
CORPUS_ROOT = REPO_ROOT / "tests" / "corpus"
PDF_DIR = CORPUS_ROOT / "pdfs"
PDFJS_RAW_BASE = "https://raw.githubusercontent.com/mozilla/pdf.js/master/test/pdfs"
PDFJS_LICENSE = "https://github.com/mozilla/pdf.js/blob/master/LICENSE"


EXISTING_FIXTURES: list[dict[str, Any]] = [
    {
        "file": "basicapi.pdf",
        "category": "text-basic",
        "notes": "Existing engine fixture; basic API text/render smoke PDF.",
    },
    {
        "file": "flate.pdf",
        "category": "text-basic",
        "notes": "Existing engine fixture with Flate-compressed text stream.",
    },
    {
        "file": "form_160f.pdf",
        "category": "forms",
        "notes": "Existing engine fixture; IRS form-style AcroForm sample.",
    },
    {
        "file": "image_only.pdf",
        "category": "scanned",
        "notes": "Existing engine fixture; image-only PDF with no text layer.",
    },
    {
        "file": "minimal.pdf",
        "category": "text-basic",
        "notes": "Existing engine fixture; minimal one-page text PDF.",
    },
    {
        "file": "multi_stream.pdf",
        "category": "text-basic",
        "notes": "Existing engine fixture with multiple content streams.",
    },
    {
        "file": "tracemonkey.pdf",
        "category": "multi-column",
        "notes": "Existing real-world fixture used by PDF.js tests.",
    },
]


PDFJS_FILES: list[dict[str, Any]] = [
    {"file": "scan-bad.pdf", "category": "scanned", "notes": "Scanned/faxed rendering edge case."},
    {"file": "images.pdf", "category": "scanned", "notes": "Image-heavy PDF.js image fixture."},
    {
        "file": "images_1bit_grayscale.pdf",
        "category": "scanned",
        "notes": "1-bit grayscale image extraction/rendering fixture.",
    },
    {
        "file": "ccitt_EndOfBlock_false.pdf",
        "category": "scanned",
        "notes": "CCITT fax stream edge case.",
    },
    {
        "file": "jbig2_symbol_offset.pdf",
        "category": "scanned",
        "notes": "JBIG2 image stream edge case.",
    },
    {"file": "xobject-image.pdf", "category": "scanned", "notes": "Image XObject rendering fixture."},
    {
        "file": "image-rotated-black-white-ratio.pdf",
        "category": "scanned",
        "notes": "Rotated black/white image fixture.",
    },
    {
        "file": "jp2k-resetprob.pdf",
        "category": "jpeg2000",
        "notes": "JPEG2000/JPXDecode fixture from PDF.js.",
    },
    {
        "file": "bug_jpx.pdf",
        "category": "jpeg2000",
        "notes": "JPEG2000/JPXDecode regression fixture from PDF.js.",
    },
    {"file": "secHandler.pdf", "category": "encrypted", "notes": "Security-handler fixture."},
    {"file": "issue14297.pdf", "category": "encrypted", "notes": "Encrypted PDF.js issue fixture."},
    {"file": "empty_protected.pdf", "category": "encrypted", "notes": "Protected empty PDF fixture."},
    {"file": "print_protection.pdf", "category": "encrypted", "notes": "Print-protected PDF fixture."},
    {
        "file": "encrypted-attachment.pdf",
        "category": "encrypted",
        "notes": "Encrypted attachment fixture.",
    },
    {
        "file": "issue15893_reduced.pdf",
        "category": "encrypted",
        "password": "test",
        "notes": "Password-protected PDF.js fixture; user password is 'test'.",
    },
    {
        "file": "gradientfill.pdf",
        "category": "complex-vector",
        "notes": "Gradient fill vector rendering fixture.",
    },
    {
        "file": "radial_gradients.pdf",
        "category": "complex-vector",
        "notes": "Radial gradients fixture.",
    },
    {
        "file": "function_based_shading.pdf",
        "category": "complex-vector",
        "notes": "Function-based shading fixture.",
    },
    {
        "file": "tiling_patterns_variations.pdf",
        "category": "complex-vector",
        "notes": "Tiling pattern variations fixture.",
    },
    {
        "file": "coons-allflags-withfunction.pdf",
        "category": "complex-vector",
        "notes": "Coons mesh shading fixture.",
    },
    {
        "file": "tensor-allflags-withfunction.pdf",
        "category": "complex-vector",
        "notes": "Tensor mesh shading fixture.",
    },
    {"file": "transparent.pdf", "category": "complex-vector", "notes": "Transparency fixture."},
    {"file": "smaskdim.pdf", "category": "complex-vector", "notes": "Soft mask dimensions fixture."},
    {
        "file": "smask_alpha_oob.pdf",
        "category": "complex-vector",
        "notes": "Soft-mask alpha out-of-bounds fixture.",
    },
    {
        "file": "knockout_groups_test.pdf",
        "category": "complex-vector",
        "notes": "Knockout groups composite-mode survey.",
    },
    {
        "file": "issue18032.pdf",
        "category": "complex-vector",
        "notes": "Nested non-isolated knockout group fixture.",
    },
    {
        "file": "annotation-text-widget.pdf",
        "category": "forms",
        "notes": "Text widget annotation fixture.",
    },
    {
        "file": "annotation-choice-widget.pdf",
        "category": "forms",
        "notes": "Choice widget annotation fixture.",
    },
    {
        "file": "annotation-button-widget.pdf",
        "category": "forms",
        "notes": "Button widget annotation fixture.",
    },
    {"file": "textfields.pdf", "category": "forms", "notes": "Text field form fixture."},
    {"file": "file_pdfjs_form.pdf", "category": "forms", "notes": "PDF.js form fixture."},
    {
        "file": "acroform_calculation_order.pdf",
        "category": "forms",
        "notes": "AcroForm calculation-order fixture.",
    },
    {"file": "form_two_pages.pdf", "category": "forms", "notes": "Two-page form fixture."},
    {"file": "prefilled_f1040.pdf", "category": "forms", "notes": "Prefilled tax form fixture."},
    {
        "file": "checkbox_no_appearance.pdf",
        "category": "forms",
        "notes": "Checkbox form without appearance stream.",
    },
    {
        "file": "bug1802506.pdf",
        "category": "forms",
        "notes": "Annotations/forms regression fixture.",
    },
    {"file": "vertical.pdf", "category": "cjk-text", "notes": "Vertical CJK text fixture."},
    {"file": "noembed-jis7.pdf", "category": "cjk-text", "notes": "JIS CJK font fixture."},
    {"file": "noembed-eucjp.pdf", "category": "cjk-text", "notes": "EUC-JP CJK font fixture."},
    {"file": "noembed-sjis.pdf", "category": "cjk-text", "notes": "Shift-JIS CJK font fixture."},
    {"file": "SimFang-variant.pdf", "category": "cjk-text", "notes": "Chinese font variant fixture."},
    {"file": "XiaoBiaoSong.pdf", "category": "cjk-text", "notes": "Chinese font fixture."},
    {
        "file": "90ms_rksj_h_sample.pdf",
        "category": "cjk-text",
        "notes": "RKSJ horizontal CJK sample.",
    },
    {
        "file": "cidfont_cmap_overflow.pdf",
        "category": "cjk-text",
        "notes": "CIDFont CMap overflow fixture.",
    },
    {
        "file": "IdentityToUnicodeMap_charCodeOf.pdf",
        "category": "cjk-text",
        "notes": "Identity ToUnicode mapping fixture.",
    },
    {"file": "issue13343.pdf", "category": "cjk-text", "notes": "CJK text regression fixture."},
    {
        "file": "ArabicCIDTrueType.pdf",
        "category": "rtl-text",
        "notes": "Arabic CID TrueType fixture.",
    },
    {"file": "ThuluthFeatures.pdf", "category": "rtl-text", "notes": "Arabic Thuluth text fixture."},
    {"file": "issue5801.pdf", "category": "rtl-text", "notes": "Mixed-direction text regression fixture."},
    {"file": "issue5874.pdf", "category": "rtl-text", "notes": "RTL/text shaping regression fixture."},
    {"file": "openoffice.pdf", "category": "multi-column", "notes": "Office-generated layout fixture."},
    {"file": "TAMReview.pdf", "category": "multi-column", "notes": "Real-world review document fixture."},
    {"file": "freeculture.pdf", "category": "multi-column", "notes": "Multi-page book-style fixture."},
    {"file": "two_paragraphs.pdf", "category": "multi-column", "notes": "Paragraph text fixture."},
    {
        "file": "paragraph_and_link.pdf",
        "category": "multi-column",
        "notes": "Paragraph and link text fixture.",
    },
    {"file": "issue20930.pdf", "category": "multi-column", "notes": "Text extraction fixture."},
    {"file": "pdfjs_wikipedia.pdf", "category": "multi-column", "notes": "Wikipedia PDF fixture."},
    {"file": "mixedfonts.pdf", "category": "multi-column", "notes": "Mixed-font text fixture."},
    {
        "file": "doc_1_3_pages.pdf",
        "category": "large-multipage",
        "notes": "PDF.js multi-page fixture.",
    },
    {
        "file": "three_pages_with_number.pdf",
        "category": "large-multipage",
        "notes": "Numbered three-page fixture.",
    },
]


def pdf_escape(text: str) -> str:
    return text.replace("\\", "\\\\").replace("(", "\\(").replace(")", "\\)")


class PdfBuilder:
    def __init__(self) -> None:
        self.objects: list[bytes | None] = [None]

    def reserve(self) -> int:
        self.objects.append(None)
        return len(self.objects) - 1

    def add(self, body: bytes | str) -> int:
        obj = self.reserve()
        self.set(obj, body)
        return obj

    def set(self, obj: int, body: bytes | str) -> None:
        if isinstance(body, str):
            body = body.encode("latin-1")
        self.objects[obj] = body

    def stream(self, data: bytes, attrs: str = "") -> int:
        attrs = attrs.strip()
        if attrs:
            attrs += " "
        body = (
            f"<< {attrs}/Length {len(data)} >>\nstream\n".encode("latin-1")
            + data
            + b"\nendstream"
        )
        return self.add(body)

    def write(self, path: Path, root_obj: int) -> None:
        output = bytearray(b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n")
        offsets = [0] * len(self.objects)
        for obj in range(1, len(self.objects)):
            body = self.objects[obj]
            if body is None:
                raise ValueError(f"object {obj} was reserved but not set")
            offsets[obj] = len(output)
            output.extend(f"{obj} 0 obj\n".encode("latin-1"))
            output.extend(body)
            output.extend(b"\nendobj\n")
        xref = len(output)
        output.extend(f"xref\n0 {len(self.objects)}\n".encode("latin-1"))
        output.extend(b"0000000000 65535 f \n")
        for offset in offsets[1:]:
            output.extend(f"{offset:010} 00000 n \n".encode("latin-1"))
        output.extend(
            f"trailer\n<< /Size {len(self.objects)} /Root {root_obj} 0 R >>\n"
            f"startxref\n{xref}\n%%EOF\n".encode("latin-1")
        )
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(bytes(output))


def text_ops(lines: list[tuple[int, int, int, str]]) -> bytes:
    ops = ["BT"]
    for x, y, size, text in lines:
        ops.append(f"/F1 {size} Tf 1 0 0 1 {x} {y} Tm ({pdf_escape(text)}) Tj")
    ops.append("ET")
    return ("\n".join(ops) + "\n").encode("latin-1")


def write_standard_pdf(path: Path, page_contents: list[bytes]) -> None:
    pdf = PdfBuilder()
    font = pdf.add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>")
    pages_obj = pdf.reserve()
    page_objs: list[int] = []
    for content in page_contents:
        content_obj = pdf.stream(content)
        page_obj = pdf.add(
            "<< /Type /Page "
            f"/Parent {pages_obj} 0 R "
            "/MediaBox [0 0 612 792] "
            f"/Resources << /Font << /F1 {font} 0 R >> >> "
            f"/Contents {content_obj} 0 R >>"
        )
        page_objs.append(page_obj)
    kids = " ".join(f"{obj} 0 R" for obj in page_objs)
    pdf.set(pages_obj, f"<< /Type /Pages /Kids [{kids}] /Count {len(page_objs)} >>")
    root = pdf.add(f"<< /Type /Catalog /Pages {pages_obj} 0 R >>")
    pdf.write(path, root)


def write_image_pdf(path: Path) -> None:
    width, height = 96, 64
    pixels = bytearray()
    for y in range(height):
        for x in range(width):
            stripe = 40 if (x // 8 + y // 8) % 2 else 220
            pixels.extend((stripe, max(0, stripe - 50), min(255, stripe + 20)))
    compressed = zlib.compress(bytes(pixels))

    pdf = PdfBuilder()
    image_obj = pdf.add(
        b"<< /Type /XObject /Subtype /Image /Width 96 /Height 64 "
        b"/ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /FlateDecode "
        + f"/Length {len(compressed)} >>\nstream\n".encode("latin-1")
        + compressed
        + b"\nendstream"
    )
    pages_obj = pdf.reserve()
    content = b"q\n460 0 0 300 72 360 cm\n/Im1 Do\nQ\n"
    content_obj = pdf.stream(content)
    page_obj = pdf.add(
        "<< /Type /Page "
        f"/Parent {pages_obj} 0 R "
        "/MediaBox [0 0 612 792] "
        f"/Resources << /XObject << /Im1 {image_obj} 0 R >> >> "
        f"/Contents {content_obj} 0 R >>"
    )
    pdf.set(pages_obj, f"<< /Type /Pages /Kids [{page_obj} 0 R] /Count 1 >>")
    root = pdf.add(f"<< /Type /Catalog /Pages {pages_obj} 0 R >>")
    pdf.write(path, root)


def write_form_pdf(path: Path) -> None:
    pdf = PdfBuilder()
    font = pdf.add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>")
    pages_obj = pdf.reserve()
    field_obj = pdf.reserve()
    content = text_ops(
        [
            (72, 720, 18, "Generated AcroForm fixture"),
            (72, 680, 12, "Name:"),
            (72, 620, 12, "This file intentionally has a text widget annotation."),
        ]
    )
    content_obj = pdf.stream(content)
    page_obj = pdf.add(
        "<< /Type /Page "
        f"/Parent {pages_obj} 0 R "
        "/MediaBox [0 0 612 792] "
        f"/Resources << /Font << /F1 {font} 0 R >> >> "
        f"/Annots [{field_obj} 0 R] "
        f"/Contents {content_obj} 0 R >>"
    )
    pdf.set(
        field_obj,
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (customer_name) "
        "/V (Ada Lovelace) /Rect [120 666 320 690] /F 4 "
        f"/P {page_obj} 0 R >>",
    )
    pdf.set(pages_obj, f"<< /Type /Pages /Kids [{page_obj} 0 R] /Count 1 >>")
    root = pdf.add(
        f"<< /Type /Catalog /Pages {pages_obj} 0 R "
        f"/AcroForm << /Fields [{field_obj} 0 R] /NeedAppearances true >> >>"
    )
    pdf.write(path, root)


def generated_entries() -> list[dict[str, Any]]:
    generated_dir = PDF_DIR / "generated"
    generated_dir.mkdir(parents=True, exist_ok=True)
    entries: list[dict[str, Any]] = []

    simple_pages = [
        text_ops(
            [
                (72, 720, 18, "Generated basic text fixture"),
                (72, 690, 12, "The quick brown fox jumps over the lazy dog."),
                (72, 670, 12, "Numbers: 1234567890 punctuation: .,:;!?"),
            ]
        )
    ]
    path = generated_dir / "generated_basic_text.pdf"
    write_standard_pdf(path, simple_pages)
    entries.append(entry_for_generated(path, "text-basic", "Generated one-page text fixture."))

    rotated = b"q\n0 1 -1 0 500 140 cm\nBT\n/F1 16 Tf 0 0 Td (Rotated text sample) Tj\nET\nQ\n"
    path = generated_dir / "generated_rotated_text.pdf"
    write_standard_pdf(path, [rotated + simple_pages[0]])
    entries.append(entry_for_generated(path, "text-basic", "Generated rotated text fixture."))

    column_lines: list[tuple[int, int, int, str]] = []
    for idx in range(16):
        column_lines.append((72, 730 - idx * 28, 10, f"Left column line {idx + 1:02d}"))
        column_lines.append((330, 730 - idx * 28, 10, f"Right column line {idx + 1:02d}"))
    path = generated_dir / "generated_two_columns.pdf"
    write_standard_pdf(path, [text_ops(column_lines)])
    entries.append(entry_for_generated(path, "multi-column", "Generated two-column text layout."))

    vector_ops = ["0.95 0.95 0.95 rg 0 0 612 792 re f"]
    for i in range(24):
        red = i / 24
        blue = 1 - red
        vector_ops.append(f"{red:.3f} 0.25 {blue:.3f} rg {40 + i * 22} 120 18 540 re f")
    vector_ops.append("0 0 0 RG 2 w 72 72 468 648 re S")
    path = generated_dir / "generated_vector_bars.pdf"
    write_standard_pdf(path, [("\n".join(vector_ops) + "\n").encode("latin-1")])
    entries.append(entry_for_generated(path, "complex-vector", "Generated vector bars and strokes."))

    path = generated_dir / "generated_image_only.pdf"
    write_image_pdf(path)
    entries.append(entry_for_generated(path, "scanned", "Generated image-only PDF."))

    path = generated_dir / "generated_form_textfield.pdf"
    write_form_pdf(path)
    entries.append(entry_for_generated(path, "forms", "Generated AcroForm text field."))

    large_pages = []
    for page in range(1, 121):
        large_pages.append(
            text_ops(
                [
                    (72, 720, 16, f"Generated large multipage fixture - page {page:03d}"),
                    (72, 690, 11, "This page exists to exercise page traversal and process overhead."),
                    (72, 670, 11, "The harness may cap render pages while text extraction still sees all pages."),
                ]
            )
        )
    path = generated_dir / "generated_120_pages.pdf"
    write_standard_pdf(path, large_pages)
    entries.append(entry_for_generated(path, "large-multipage", "Generated 120-page performance baseline."))

    rtl_text = [
        (72, 720, 16, "Generated mixed direction placeholder"),
        (72, 690, 12, "Arabic/Hebrew coverage is primarily from PDF.js fixtures."),
        (72, 670, 12, "Mixed Latin markers: START abc 123 END"),
    ]
    path = generated_dir / "generated_rtl_placeholder.pdf"
    write_standard_pdf(path, [text_ops(rtl_text)])
    entries.append(entry_for_generated(path, "rtl-text", "Generated mixed-direction placeholder."))

    return entries


def entry_for_generated(path: Path, category: str, notes: str) -> dict[str, Any]:
    return {
        "id": path.stem,
        "path": str(path.relative_to(REPO_ROOT)).replace("\\", "/"),
        "category": category,
        "source": "generated",
        "license": "CC0-1.0",
        "notes": notes,
        "sha256": sha256(path),
        "size_bytes": path.stat().st_size,
    }


def copy_existing_fixtures() -> list[dict[str, Any]]:
    out_dir = PDF_DIR / "existing"
    out_dir.mkdir(parents=True, exist_ok=True)
    entries = []
    for item in EXISTING_FIXTURES:
        source = REPO_ROOT / "crates" / "engine" / "tests" / "fixtures" / item["file"]
        if not source.exists():
            print(f"missing existing fixture: {source}", file=sys.stderr)
            continue
        dest = out_dir / item["file"]
        shutil.copy2(source, dest)
        entries.append(
            {
                "id": "existing_" + dest.stem,
                "path": str(dest.relative_to(REPO_ROOT)).replace("\\", "/"),
                "category": item["category"],
                "source": "existing-engine-fixture",
                "license": "project fixture",
                "notes": item["notes"],
                "sha256": sha256(dest),
                "size_bytes": dest.stat().st_size,
            }
        )
    return entries


def download_pdfjs_files(allow_missing: bool) -> list[dict[str, Any]]:
    out_dir = PDF_DIR / "pdfjs"
    out_dir.mkdir(parents=True, exist_ok=True)
    entries = []
    failures = []
    for item in PDFJS_FILES:
        filename = item["file"]
        dest = out_dir / filename
        url = f"{PDFJS_RAW_BASE}/{filename}"
        try:
            if not dest.exists():
                print(f"download {filename}", file=sys.stderr)
                with urllib.request.urlopen(url, timeout=60) as response:
                    dest.write_bytes(response.read())
            if not dest.read_bytes().startswith(b"%PDF-"):
                raise ValueError("downloaded file does not start with %PDF-")
        except (urllib.error.URLError, TimeoutError, ValueError) as err:
            failures.append((filename, str(err)))
            if dest.exists():
                dest.unlink()
            continue
        entry = {
            "id": "pdfjs_" + Path(filename).stem.replace(".", "_"),
            "path": str(dest.relative_to(REPO_ROOT)).replace("\\", "/"),
            "category": item["category"],
            "source": "mozilla/pdf.js test/pdfs",
            "source_url": url,
            "license": "Apache-2.0",
            "license_url": PDFJS_LICENSE,
            "notes": item["notes"],
            "sha256": sha256(dest),
            "size_bytes": dest.stat().st_size,
        }
        if "password" in item:
            entry["password"] = item["password"]
        entries.append(entry)
    if failures:
        for filename, error in failures:
            print(f"failed {filename}: {error}", file=sys.stderr)
        if not allow_missing:
            raise SystemExit(f"{len(failures)} PDF.js corpus downloads failed")
    return entries


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def write_manifest(entries: list[dict[str, Any]]) -> None:
    categories = sorted({entry["category"] for entry in entries})
    manifest = {
        "version": 1,
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "description": "Tagged corpus for Poppler-vs-Oxide parity measurements.",
        "sources": {
            "existing-engine-fixture": {
                "path": "crates/engine/tests/fixtures",
                "license": "project fixture",
            },
            "mozilla/pdf.js test/pdfs": {
                "url": "https://github.com/mozilla/pdf.js/tree/master/test/pdfs",
                "license": "Apache-2.0",
                "license_url": PDFJS_LICENSE,
            },
            "generated": {
                "path": "scripts/generate_parity_corpus.py",
                "license": "CC0-1.0",
            },
        },
        "categories": categories,
        "entries": sorted(entries, key=lambda entry: (entry["category"], entry["id"])),
    }
    CORPUS_ROOT.mkdir(parents=True, exist_ok=True)
    (CORPUS_ROOT / "manifest.json").write_text(
        json.dumps(manifest, indent=2, ensure_ascii=False) + "\n",
        encoding="utf-8",
    )


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--allow-missing", action="store_true")
    parser.add_argument("--skip-downloads", action="store_true")
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    entries = []
    entries.extend(copy_existing_fixtures())
    if not args.skip_downloads:
        entries.extend(download_pdfjs_files(allow_missing=args.allow_missing))
    entries.extend(generated_entries())
    if len(entries) < 50:
        raise SystemExit(f"corpus has only {len(entries)} files; expected at least 50")
    write_manifest(entries)
    print(f"wrote {CORPUS_ROOT / 'manifest.json'} with {len(entries)} entries")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
