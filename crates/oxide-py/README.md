# oxide-py (Deferred)

Python bindings are intentionally deferred to a PyO3/maturin follow-up. This
directory is a scope marker, not a buildable crate.

Planned API:

```python
from oxide_pdf import Document

doc = Document.from_bytes(pdf_bytes)
print(doc.page_count())
print(doc.extract_text(1))
png = doc.render_page_png(1, dpi=150)
semantic = doc.extract_semantic()
```

Implementation notes:

- Use a separate PyO3 crate with maturin packaging.
- Convert `oxide-engine` errors to Python exceptions.
- Return Python-native `str`, `bytes`, `dict`, and `list` values.
- Start with bytes/page-count/text/info/render/semantic; add images,
  attachments, signatures, and manipulation after the core wrapper is stable.
