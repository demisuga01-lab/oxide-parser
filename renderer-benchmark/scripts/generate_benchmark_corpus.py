#!/usr/bin/env python3
"""Generate the seed corpus for Renderer Benchmark 0A.

The generated PDFs are intentionally minimal and deterministic. They are not a
replacement for a 1,000+ file real-world corpus; they give the harness isolated
coverage and hostile inputs that can be regenerated locally.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import zlib
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]
BENCH_ROOT = REPO_ROOT / "renderer-benchmark"
CORPUS_ROOT = BENCH_ROOT / "corpus"


@dataclass
class Entry:
    id: str
    path: Path
    category: str
    source: str
    notes: str
    source_url: str | None = None
    license: str | None = None
    license_url: str | None = None


class PdfBuilder:
    def __init__(self) -> None:
        self.objects: list[bytes] = []

    def add(self, body: str | bytes) -> int:
        if isinstance(body, str):
            body = body.encode("latin-1")
        self.objects.append(body)
        return len(self.objects)

    def stream(self, dict_src: str, data: bytes | str) -> bytes:
        if isinstance(data, str):
            data = data.encode("latin-1")
        return (
            f"<< {dict_src} /Length {len(data)} >>\nstream\n".encode("latin-1")
            + data
            + b"\nendstream"
        )

    def write(self, path: Path, root_obj: int) -> None:
        out = bytearray(b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n")
        offsets = [0]
        for i, body in enumerate(self.objects, start=1):
            offsets.append(len(out))
            out.extend(f"{i} 0 obj\n".encode("latin-1"))
            out.extend(body)
            out.extend(b"\nendobj\n")
        startxref = len(out)
        out.extend(f"xref\n0 {len(self.objects) + 1}\n".encode("latin-1"))
        out.extend(b"0000000000 65535 f \n")
        for offset in offsets[1:]:
            out.extend(f"{offset:010d} 00000 n \n".encode("latin-1"))
        out.extend(
            (
                f"trailer\n<< /Size {len(self.objects) + 1} /Root {root_obj} 0 R >>\n"
                f"startxref\n{startxref}\n%%EOF\n"
            ).encode("latin-1")
        )
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(bytes(out))


def pdf_literal(text: str) -> str:
    return "(" + text.replace("\\", "\\\\").replace("(", "\\(").replace(")", "\\)") + ")"


def make_document(
    path: Path,
    page_streams: list[str],
    *,
    mediabox: tuple[int, int, int, int] = (0, 0, 612, 792),
    rotate: int | None = None,
    cropbox: tuple[int, int, int, int] | None = None,
    resources_extra: str = "",
    page_extra: str = "",
    catalog_extra: str = "",
    extra_objects: list[str | bytes] | None = None,
) -> None:
    b = PdfBuilder()
    font = b.add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>")
    extras = [b.add(obj) for obj in (extra_objects or [])]

    resource_parts = [f"/Font << /F1 {font} 0 R >>"]
    if resources_extra:
        resource_parts.append(resources_extra.format(*extras))
    resources = "<< " + " ".join(resource_parts) + " >>"

    content_refs = [b.add(b.stream("", stream)) for stream in page_streams]
    pages_obj = len(b.objects) + len(content_refs) + 1
    page_refs: list[int] = []
    for content_ref in content_refs:
        page_dict = (
            f"<< /Type /Page /Parent {pages_obj} 0 R "
            f"/MediaBox [{mediabox[0]} {mediabox[1]} {mediabox[2]} {mediabox[3]}] "
            f"/Resources {resources} /Contents {content_ref} 0 R"
        )
        if rotate is not None:
            page_dict += f" /Rotate {rotate}"
        if cropbox is not None:
            page_dict += f" /CropBox [{cropbox[0]} {cropbox[1]} {cropbox[2]} {cropbox[3]}]"
        if page_extra:
            page_dict += " " + page_extra.format(*extras)
        page_dict += " >>"
        page_refs.append(b.add(page_dict))
    kids = " ".join(f"{ref} 0 R" for ref in page_refs)
    b.add(f"<< /Type /Pages /Kids [{kids}] /Count {len(page_refs)} >>")
    root = b.add(f"<< /Type /Catalog /Pages {pages_obj} 0 R {catalog_extra.format(*extras)} >>")
    b.write(path, root)


def base_text(label: str, y: int = 720) -> str:
    return f"BT /F1 20 Tf 72 {y} Td {pdf_literal(label)} Tj ET\n"


def make_image_xobject() -> bytes:
    # 3x3 RGB checker image, Flate-encoded.
    pixels = bytes(
        [
            255,
            0,
            0,
            0,
            255,
            0,
            0,
            0,
            255,
            255,
            255,
            0,
            0,
            255,
            255,
            255,
            0,
            255,
            40,
            40,
            40,
            180,
            180,
            180,
            255,
            255,
            255,
        ]
    )
    data = zlib.compress(pixels)
    return (
        b"<< /Type /XObject /Subtype /Image /Width 3 /Height 3 "
        b"/ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /FlateDecode "
        + f"/Length {len(data)} >>\nstream\n".encode("latin-1")
        + data
        + b"\nendstream"
    )


def synthetic_specs() -> list[tuple[str, str, str, dict[str, object]]]:
    specs: list[tuple[str, str, str, dict[str, object]]] = []
    colors = [
        ("rgb-red", "1 0 0 rg 72 560 120 80 re f\n"),
        ("rgb-green", "0 0.7 0 rg 72 560 120 80 re f\n"),
        ("rgb-blue", "0 0 1 rg 72 560 120 80 re f\n"),
        ("cmyk", "0 0.8 0.8 0 k 72 560 120 80 re f\n"),
    ]
    for idx in range(24):
        color_name, paint = colors[idx % len(colors)]
        stream = (
            base_text(f"synthetic text graphics {idx}")
            + paint
            + "0 0 0 RG 2 w 72 500 m 220 610 l 300 520 l S\n"
            + "1 0 0 1 260 480 cm 0.2 0.6 0.9 rg 0 0 80 50 re f\n"
        )
        specs.append((f"synthetic_graphics_{idx:03}_{color_name}", "synthetic-graphics", stream, {}))

    for idx, rotate in enumerate([0, 90, 180, 270] * 6):
        stream = base_text(f"synthetic geometry rotate {rotate}", 700)
        specs.append(
            (
                f"synthetic_geometry_rotate_{idx:03}",
                "synthetic-geometry",
                stream,
                {"rotate": rotate, "cropbox": (20, 20, 592, 772) if idx % 2 else None},
            )
        )

    for idx in range(18):
        mode = idx % 8
        stream = (
            base_text(f"synthetic text mode {mode}", 720)
            + f"BT /F1 32 Tf {mode} Tr 72 640 Td {pdf_literal('Mode ' + str(mode))} Tj ET\n"
            + "0 Tr\n"
        )
        specs.append((f"synthetic_text_mode_{idx:03}", "synthetic-text", stream, {}))

    for idx in range(18):
        stream = (
            base_text(f"synthetic clip curve {idx}", 720)
            + "q 80 500 180 120 re W n 0.9 0.2 0.1 rg "
            + "80 500 m 120 700 220 420 300 620 c 300 500 l f Q\n"
            + "[6 3] 0 d 3 w 0 0 0 RG 72 460 280 90 re S\n"
        )
        specs.append((f"synthetic_clip_curve_{idx:03}", "synthetic-graphics", stream, {}))

    image = make_image_xobject()
    for idx in range(16):
        stream = (
            base_text(f"synthetic image {idx}", 720)
            + "q 150 0 0 150 80 470 cm /Im1 Do Q\n"
            + "q 80 0 0 120 300 500 cm /Im1 Do Q\n"
        )
        specs.append(
            (
                f"synthetic_image_{idx:03}",
                "synthetic-images",
                stream,
                {"extra_objects": [image], "resources_extra": "/XObject << /Im1 {0} 0 R >>"},
            )
        )

    for idx in range(12):
        stream = (
            base_text(f"synthetic transparency {idx}", 720)
            + "q /GS1 gs 1 0 0 rg 72 520 170 120 re f Q\n"
            + "q /GS2 gs 0 0 1 rg 150 560 170 120 re f Q\n"
        )
        gs1 = "<< /Type /ExtGState /ca 0.45 /CA 0.45 /BM /Multiply >>"
        gs2 = "<< /Type /ExtGState /ca 0.55 /CA 0.55 /BM /Screen >>"
        specs.append(
            (
                f"synthetic_transparency_{idx:03}",
                "synthetic-transparency",
                stream,
                {"extra_objects": [gs1, gs2], "resources_extra": "/ExtGState << /GS1 {0} 0 R /GS2 {1} 0 R >>"},
            )
        )

    for idx in range(10):
        form_stream = PdfBuilder().stream(
            "/Type /XObject /Subtype /Form /BBox [0 0 120 60] /Resources << >>",
            "0.1 0.5 0.9 rg 0 0 120 60 re f 0 0 0 RG 3 w 0 0 120 60 re S",
        )
        stream = base_text(f"synthetic form xobject {idx}", 720) + "q 2 0 0 2 80 480 cm /Fm1 Do Q\n"
        specs.append(
            (
                f"synthetic_form_{idx:03}",
                "synthetic-forms",
                stream,
                {"extra_objects": [form_stream], "resources_extra": "/XObject << /Fm1 {0} 0 R >>"},
            )
        )

    # 120 entries total.
    return specs[:120]


def generate_synthetic(clean: bool) -> list[Entry]:
    out_dir = CORPUS_ROOT / "synthetic"
    if clean and out_dir.exists():
        shutil.rmtree(out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    entries: list[Entry] = []
    for doc_id, category, stream, options in synthetic_specs():
        path = out_dir / f"{doc_id}.pdf"
        make_document(path, [stream], **options)
        entries.append(Entry(doc_id, path, category, "renderer-benchmark generator", "minimal isolated synthetic PDF"))
    return entries


def write_hostile_bytes(path: Path, kind: str, idx: int) -> str:
    path.parent.mkdir(parents=True, exist_ok=True)
    if kind == "random":
        path.write_bytes(b"%PDF-1.7\n" + bytes((i * 17 + idx) % 256 for i in range(128)))
        return "random bytes with PDF header"
    if kind == "truncated":
        path.write_bytes(b"%PDF-1.7\n1 0 obj\n<< /Type /Catalog")
        return "truncated object"
    if kind == "wrong-startxref":
        make_document(path, [base_text("wrong startxref")])
        data = path.read_bytes().replace(b"startxref\n", b"startxref\n999999\n%")
        path.write_bytes(data)
        return "wrong startxref offset"
    if kind == "missing-eof":
        make_document(path, [base_text("missing eof")])
        path.write_bytes(path.read_bytes().replace(b"%%EOF\n", b""))
        return "missing EOF marker"
    if kind == "huge-page":
        make_document(path, [base_text("huge page")], mediabox=(0, 0, 200000, 200000))
        return "huge declared page dimensions"
    if kind == "bad-filter":
        b = PdfBuilder()
        stream = b.add(b"<< /Length 8 /Filter /DefinitelyNotAFilter >>\nstream\nABCDEFGH\nendstream")
        pages = b.add(f"<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
        b.add(f"<< /Type /Page /Parent {pages} 0 R /MediaBox [0 0 612 792] /Contents {stream} 0 R >>")
        root = b.add(f"<< /Type /Catalog /Pages {pages} 0 R >>")
        b.write(path, root)
        return "invalid stream filter"
    if kind == "huge-length":
        path.write_bytes(
            b"%PDF-1.7\n1 0 obj\n<< /Length 999999999 /Filter /FlateDecode >>\nstream\nx\x9c\x03\x00\x00\x00\x00\x01\nendstream\nendobj\nstartxref\n0\n%%EOF\n"
        )
        return "malicious huge stream length"
    if kind == "openaction-js":
        js = "<< /S /JavaScript /JS (app.alert('no execution expected')) >>"
        make_document(path, [base_text("active content ignored")], extra_objects=[js], catalog_extra="/OpenAction {0} 0 R")
        return "OpenAction JavaScript should be ignored"
    if kind == "uri-action":
        annot = "<< /Type /Annot /Subtype /Link /Rect [70 690 260 730] /A << /S /URI /URI (https://example.invalid) >> >>"
        make_document(path, [base_text("URI action ignored")], extra_objects=[annot], page_extra="/Annots [{0} 0 R]")
        return "URI action should be ignored"
    if kind == "launch-action":
        annot = "<< /Type /Annot /Subtype /Link /Rect [70 690 260 730] /A << /S /Launch /F (calc.exe) >> >>"
        make_document(path, [base_text("Launch action ignored")], extra_objects=[annot], page_extra="/Annots [{0} 0 R]")
        return "Launch action should be ignored"
    raise ValueError(kind)


def generate_hostile(clean: bool) -> list[Entry]:
    out_dir = CORPUS_ROOT / "hostile"
    if clean and out_dir.exists():
        shutil.rmtree(out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    kinds = [
        "random",
        "truncated",
        "wrong-startxref",
        "missing-eof",
        "huge-page",
        "bad-filter",
        "huge-length",
        "openaction-js",
        "uri-action",
        "launch-action",
    ]
    entries: list[Entry] = []
    for idx in range(60):
        kind = kinds[idx % len(kinds)]
        doc_id = f"hostile_{idx:03}_{kind}"
        path = out_dir / f"{doc_id}.pdf"
        notes = write_hostile_bytes(path, kind, idx)
        entries.append(Entry(doc_id, path, f"hostile-{kind}", "renderer-benchmark generator", notes))
    return entries


def generate_large(clean: bool) -> list[Entry]:
    out_dir = CORPUS_ROOT / "large-files"
    if clean and out_dir.exists():
        shutil.rmtree(out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    page_counts = [10, 20, 30, 40, 50, 75, 100, 120, 150, 200]
    entries: list[Entry] = []
    for count in page_counts:
        doc_id = f"large_{count:03}_pages"
        path = out_dir / f"{doc_id}.pdf"
        streams = [
            base_text(f"large file {count} pages - page {page}", 720)
            + "0.2 0.3 0.7 rg 72 520 220 80 re f\n"
            for page in range(1, count + 1)
        ]
        make_document(path, streams)
        entries.append(Entry(doc_id, path, "large-files", "renderer-benchmark generator", f"{count} generated pages"))
    return entries


def real_world_entries() -> list[Entry]:
    entries: list[Entry] = []
    manifest_path = REPO_ROOT / "tests" / "corpus" / "manifest.json"
    if manifest_path.exists():
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        for raw in manifest.get("entries", []):
            path = REPO_ROOT / raw["path"]
            if path.exists():
                entries.append(
                    Entry(
                        id=f"real_{raw['id']}",
                        path=path,
                        category=f"real-{raw.get('category', 'uncategorized')}",
                        source=raw.get("source", "tests/corpus"),
                        notes=raw.get("notes", "existing corpus file"),
                        source_url=raw.get("source_url"),
                        license=raw.get("license"),
                        license_url=raw.get("license_url"),
                    )
                )

    real_dir = CORPUS_ROOT / "real-world"
    metadata_path = CORPUS_ROOT / "real-world-sources.json"
    metadata: dict[str, dict[str, object]] = {}
    if metadata_path.exists():
        raw_metadata = json.loads(metadata_path.read_text(encoding="utf-8"))
        metadata = {
            str(item.get("path", "")).replace("\\", "/"): item
            for item in raw_metadata.get("entries", [])
            if item.get("path")
        }
    for pdf in sorted(real_dir.rglob("*.pdf")):
        rel = str(pdf.relative_to(REPO_ROOT)).replace("\\", "/")
        meta = metadata.get(rel, {})
        entries.append(
            Entry(
                id=str(meta.get("id") or f"user_{pdf.stem}"),
                path=pdf,
                category=str(meta.get("category") or "real-user-supplied"),
                source=str(meta.get("source") or "renderer-benchmark/corpus/real-world"),
                notes=str(meta.get("notes") or "user-supplied real-world PDF"),
                source_url=meta.get("source_url") if isinstance(meta.get("source_url"), str) else None,
                license=meta.get("license") if isinstance(meta.get("license"), str) else None,
                license_url=meta.get("license_url") if isinstance(meta.get("license_url"), str) else None,
            )
        )

    deduped: list[Entry] = []
    seen: set[str] = set()
    for entry in entries:
        try:
            digest = hashlib.sha256(entry.path.read_bytes()).hexdigest()
        except OSError:
            continue
        if digest in seen:
            continue
        seen.add(digest)
        deduped.append(entry)
    return deduped


def manifest_entry(entry: Entry) -> dict[str, object]:
    data = entry.path.read_bytes() if entry.path.exists() else b""
    out: dict[str, object] = {
        "id": entry.id,
        "path": str(entry.path.relative_to(REPO_ROOT)).replace("\\", "/"),
        "category": entry.category,
        "source": entry.source,
        "notes": entry.notes,
        "sha256": hashlib.sha256(data).hexdigest(),
        "size_bytes": len(data),
    }
    if entry.source_url:
        out["source_url"] = entry.source_url
    if entry.license:
        out["license"] = entry.license
    if entry.license_url:
        out["license_url"] = entry.license_url
    return out


def write_manifest(entries: list[Entry]) -> Path:
    manifest = {
        "version": 1,
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "description": "Renderer Benchmark 0A seed corpus. Expand real-world PDFs before claiming high-tier evidence.",
        "targets": {
            "full_spec_real_world_files": 1000,
            "full_spec_rendered_pages": 10000,
        },
        "counts": {
            "synthetic": sum(e.category.startswith("synthetic-") for e in entries),
            "hostile": sum(e.category.startswith("hostile-") for e in entries),
            "large": sum(e.category == "large-files" for e in entries),
            "real_world": sum(e.category.startswith("real-") for e in entries),
            "total": len(entries),
        },
        "entries": [manifest_entry(e) for e in entries],
    }
    path = CORPUS_ROOT / "manifest.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    return path


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--no-clean", action="store_true", help="do not replace generated synthetic/hostile/large directories")
    args = parser.parse_args()

    clean = not args.no_clean
    entries: list[Entry] = []
    entries.extend(generate_synthetic(clean))
    entries.extend(generate_hostile(clean))
    entries.extend(generate_large(clean))
    entries.extend(real_world_entries())
    manifest = write_manifest(entries)

    counts: dict[str, int] = {}
    for entry in entries:
        family = entry.category.split("-", 1)[0]
        counts[family] = counts.get(family, 0) + 1
    print(f"Wrote {manifest}")
    print(json.dumps({"total": len(entries), **counts}, indent=2))


if __name__ == "__main__":
    main()
