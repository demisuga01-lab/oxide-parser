#!/usr/bin/env python3
"""Generate minimal PDF fixtures with embedded file attachments.

Produces deterministic, byte-exact fixtures under
crates/engine/tests/fixtures/ for the `detach` (pdfdetach-equivalent) tests:

  attach_nametree.pdf   - one embedded file via /Names /EmbeddedFiles
  attach_annot.pdf      - one embedded file via a /FileAttachment annotation
  attach_traversal.pdf  - embedded file whose name is "../../evil.txt"
  attach_none.pdf       - a valid PDF with no attachments

Each embedded payload is stored UNCOMPRESSED (no /Filter) so the test knows the
exact bytes, and the name-tree fixture carries a correct /Params /CheckSum
(MD5 of the payload) so checksum verification can be exercised.
"""
import hashlib
import os

FIXTURES = os.path.join(os.path.dirname(__file__), "..", "crates", "engine", "tests", "fixtures")


def build_pdf(objects, root_obj):
    """objects: list of bytes bodies indexed 1..N (objects[0] is object 1).
    Returns the full PDF bytes with a correct classic xref table."""
    header = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n"
    out = bytearray(header)
    offsets = [0] * (len(objects) + 1)
    for i, body in enumerate(objects, start=1):
        offsets[i] = len(out)
        out += f"{i} 0 obj\n".encode("latin1")
        out += body
        out += b"\nendobj\n"
    xref_off = len(out)
    n = len(objects) + 1
    out += b"xref\n"
    out += f"0 {n}\n".encode("latin1")
    out += b"0000000000 65535 f \n"
    for i in range(1, n):
        out += f"{offsets[i]:010d} 00000 n \n".encode("latin1")
    out += b"trailer\n"
    out += f"<< /Size {n} /Root {root_obj} 0 R >>\n".encode("latin1")
    out += b"startxref\n"
    out += f"{xref_off}\n".encode("latin1")
    out += b"%%EOF\n"
    return bytes(out)


def embedded_file_stream(payload, with_checksum=True):
    """An /EmbeddedFile stream object body (uncompressed)."""
    params = f"/Size {len(payload)}"
    if with_checksum:
        md5 = hashlib.md5(payload).digest()
        # PDF hex string for the checksum.
        hexsum = "".join(f"{b:02X}" for b in md5)
        params += f" /CheckSum <{hexsum}>"
    body = (
        f"<< /Type /EmbeddedFile /Length {len(payload)} /Params << {params} >> >>\n".encode("latin1")
        + b"stream\n"
        + payload
        + b"\nendstream"
    )
    return body


def filespec(name, ef_obj):
    return f"<< /Type /Filespec /F ({name}) /UF ({name}) /EF << /F {ef_obj} 0 R >> /Desc (test attachment) >>".encode("latin1")


def page_obj(parent_obj, annots=None):
    extra = f" /Annots [ {annots} ]" if annots else ""
    return f"<< /Type /Page /Parent {parent_obj} 0 R /MediaBox [0 0 200 200]{extra} >>".encode("latin1")


def make_nametree(payload):
    # 1 catalog, 2 pages, 3 page, 4 embedded-file stream, 5 filespec
    catalog = b"<< /Type /Catalog /Pages 2 0 R /Names << /EmbeddedFiles << /Names [ (Hello.txt) 5 0 R ] >> >> >>"
    pages = b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"
    page = page_obj(2)
    ef = embedded_file_stream(payload, with_checksum=True)
    fs = filespec("Hello.txt", 4)
    return build_pdf([catalog, pages, page, ef, fs], root_obj=1)


def make_annotation(payload):
    # 1 catalog, 2 pages, 3 page(with annots), 4 ef stream, 5 filespec, 6 annot
    catalog = b"<< /Type /Catalog /Pages 2 0 R >>"
    pages = b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"
    page = page_obj(2, annots="6 0 R")
    ef = embedded_file_stream(payload, with_checksum=False)
    fs = filespec("annot-data.bin", 4)
    annot = b"<< /Type /Annot /Subtype /FileAttachment /Rect [10 10 30 30] /FS 5 0 R >>"
    return build_pdf([catalog, pages, page, ef, fs, annot], root_obj=1)


def make_traversal(payload):
    catalog = b"<< /Type /Catalog /Pages 2 0 R /Names << /EmbeddedFiles << /Names [ (evil) 5 0 R ] >> >> >>"
    pages = b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"
    page = page_obj(2)
    ef = embedded_file_stream(payload, with_checksum=False)
    # Malicious name: parent-dir traversal.
    fs = filespec("../../evil.txt", 4)
    return build_pdf([catalog, pages, page, ef, fs], root_obj=1)


def make_none():
    catalog = b"<< /Type /Catalog /Pages 2 0 R >>"
    pages = b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"
    page = page_obj(2)
    return build_pdf([catalog, pages, page], root_obj=1)


def main():
    os.makedirs(FIXTURES, exist_ok=True)
    nt_payload = b"Hello, embedded world!\nThis is a name-tree attachment.\n"
    an_payload = bytes(range(0, 256)) * 2  # binary payload, 512 bytes
    tv_payload = b"pretend this escaped the directory"

    fixtures = {
        "attach_nametree.pdf": make_nametree(nt_payload),
        "attach_annot.pdf": make_annotation(an_payload),
        "attach_traversal.pdf": make_traversal(tv_payload),
        "attach_none.pdf": make_none(),
    }
    for name, data in fixtures.items():
        path = os.path.join(FIXTURES, name)
        with open(path, "wb") as f:
            f.write(data)
        print(f"wrote {path} ({len(data)} bytes)")

    # Print the known payloads' MD5 so the tests can hardcode expectations.
    print("nametree payload md5:", hashlib.md5(nt_payload).hexdigest())
    print("nametree payload len:", len(nt_payload))
    print("annot payload len:", len(an_payload))


if __name__ == "__main__":
    main()
