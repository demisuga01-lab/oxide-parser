//! C ABI for oxide-engine.

use std::ffi::CString;
use std::os::raw::{c_char, c_int};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;
use std::slice;

use oxide_engine::{
    ContentEngine, DocType, ExtractOptions, ParseOptions, Result as OxideResult, TextExtractor,
};

pub const OXIDE_STATUS_OK: c_int = 0;
pub const OXIDE_STATUS_NULL: c_int = 1;
pub const OXIDE_STATUS_ERROR: c_int = 2;
pub const OXIDE_STATUS_PANIC: c_int = 3;

#[repr(C)]
pub struct OxideDocument {
    engine: ContentEngine,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct OxideBuffer {
    pub data: *mut u8,
    pub len: usize,
}

impl OxideBuffer {
    fn empty() -> Self {
        Self {
            data: ptr::null_mut(),
            len: 0,
        }
    }
}

/// Opens a PDF document from an in-memory byte slice.
///
/// # Safety
///
/// `data` must point to `len` readable bytes. If `error_out` is non-null, it
/// must be writable and any returned string must be freed with
/// `oxide_error_free`.
#[no_mangle]
pub unsafe extern "C" fn oxide_document_open_from_bytes(
    data: *const u8,
    len: usize,
    error_out: *mut *mut c_char,
) -> *mut OxideDocument {
    clear_error(error_out);
    if data.is_null() {
        set_error(error_out, "data pointer is null");
        return ptr::null_mut();
    }

    match catch_unwind(AssertUnwindSafe(|| {
        let bytes = unsafe { slice::from_raw_parts(data, len) }.to_vec();
        ContentEngine::open_bytes(bytes)
    })) {
        Ok(Ok(engine)) => Box::into_raw(Box::new(OxideDocument { engine })),
        Ok(Err(err)) => {
            set_error(error_out, &err.to_string());
            ptr::null_mut()
        }
        Err(_) => {
            set_error(error_out, "panic while opening document");
            ptr::null_mut()
        }
    }
}

/// Frees a document returned by `oxide_document_open_from_bytes`.
///
/// # Safety
///
/// `document` must be null or a pointer returned by
/// `oxide_document_open_from_bytes` that has not already been freed.
#[no_mangle]
pub unsafe extern "C" fn oxide_document_free(document: *mut OxideDocument) {
    if !document.is_null() {
        let _ = unsafe { Box::from_raw(document) };
    }
}

/// Frees a UTF-8 string returned by this C API.
///
/// # Safety
///
/// `value` must be null or a pointer returned by an oxide C-API string
/// function that has not already been freed.
#[no_mangle]
pub unsafe extern "C" fn oxide_string_free(value: *mut c_char) {
    if !value.is_null() {
        let _ = unsafe { CString::from_raw(value) };
    }
}

/// Frees an error string returned through an `error_out` parameter.
///
/// # Safety
///
/// `value` must be null or a pointer returned through an oxide C-API
/// `error_out` parameter that has not already been freed.
#[no_mangle]
pub unsafe extern "C" fn oxide_error_free(value: *mut c_char) {
    unsafe { oxide_string_free(value) };
}

/// Frees a byte buffer returned by this C API.
///
/// # Safety
///
/// `buffer` must be empty or a buffer returned by an oxide C-API function that
/// has not already been freed.
#[no_mangle]
pub unsafe extern "C" fn oxide_buffer_free(buffer: OxideBuffer) {
    if !buffer.data.is_null() && buffer.len > 0 {
        let slice = ptr::slice_from_raw_parts_mut(buffer.data, buffer.len);
        let _ = unsafe { Box::from_raw(slice) };
    }
}

/// Returns the number of pages in a document.
///
/// # Safety
///
/// `document` must be a valid open document. `out_count` must be writable. If
/// `error_out` is non-null, it must be writable and any returned string must be
/// freed with `oxide_error_free`.
#[no_mangle]
pub unsafe extern "C" fn oxide_document_page_count(
    document: *const OxideDocument,
    out_count: *mut usize,
    error_out: *mut *mut c_char,
) -> c_int {
    ffi_status(error_out, || {
        let doc = checked_doc(document)?;
        if out_count.is_null() {
            return Err("out_count pointer is null".into());
        }
        unsafe {
            *out_count = oxide(doc.engine.page_count())?;
        }
        Ok(())
    })
}

/// Extracts text from a document.
///
/// # Safety
///
/// `document` must be a valid open document. `out_text` must be writable and
/// any returned string must be freed with `oxide_string_free`. If `error_out`
/// is non-null, it must be writable and any returned string must be freed with
/// `oxide_error_free`.
#[no_mangle]
pub unsafe extern "C" fn oxide_document_extract_text(
    document: *const OxideDocument,
    page: usize,
    out_text: *mut *mut c_char,
    error_out: *mut *mut c_char,
) -> c_int {
    ffi_status(error_out, || {
        let doc = checked_doc(document)?;
        if out_text.is_null() {
            return Err("out_text pointer is null".into());
        }
        let text = if page == 0 {
            oxide(TextExtractor::extract_default(&doc.engine))?
        } else {
            oxide(doc.engine.get_page_text(page))?
        };
        unsafe {
            *out_text = into_c_string(text);
        }
        Ok(())
    })
}

/// Extracts the tags-first semantic document as JSON.
///
/// # Safety
///
/// `document` must be a valid open document. `out_json` must be writable and
/// any returned string must be freed with `oxide_string_free`. If `error_out`
/// is non-null, it must be writable and any returned string must be freed with
/// `oxide_error_free`.
#[no_mangle]
pub unsafe extern "C" fn oxide_document_extract_semantic_json(
    document: *const OxideDocument,
    out_json: *mut *mut c_char,
    error_out: *mut *mut c_char,
) -> c_int {
    ffi_status(error_out, || {
        let doc = checked_doc(document)?;
        if out_json.is_null() {
            return Err("out_json pointer is null".into());
        }
        let semantic = oxide(doc.engine.extract_semantic_document(&[]))?;
        let json = serde_json::to_string(&semantic).map_err(|err| err.to_string())?;
        unsafe {
            *out_json = into_c_string(json);
        }
        Ok(())
    })
}

/// Parses the document into the canonical model and renders it as Markdown.
///
/// This is the AI/RAG-facing parser output: headings, paragraphs, lists,
/// tables, figures, and captions in recovered reading order. Uses default
/// parse options over all pages. Digital-born only — scanned pages degrade to a
/// placeholder (OCR is not wired through the C ABI).
///
/// # Safety
///
/// `document` must be a valid open document. `out_markdown` must be writable and
/// any returned string must be freed with `oxide_string_free`. If `error_out`
/// is non-null, it must be writable and any returned string must be freed with
/// `oxide_error_free`.
#[no_mangle]
pub unsafe extern "C" fn oxide_document_parse_markdown(
    document: *const OxideDocument,
    out_markdown: *mut *mut c_char,
    error_out: *mut *mut c_char,
) -> c_int {
    ffi_status(error_out, || {
        let doc = checked_doc(document)?;
        if out_markdown.is_null() {
            return Err("out_markdown pointer is null".into());
        }
        let parsed = oxide(doc.engine.parse_document(&ParseOptions::default()))?;
        let markdown = parsed.to_markdown_default();
        unsafe {
            *out_markdown = into_c_string(markdown);
        }
        Ok(())
    })
}

/// Parses the document into the canonical [`Document`] model and returns it as
/// JSON. This is the SAME schema (1.1) the CLI `parse --format json`, the
/// server `/parse` endpoint, and the WASM `parseJson` binding emit — the single
/// canonical structured output. (Distinct from
/// `oxide_document_extract_semantic_json`, which serializes the older semantic
/// model and is retained only for back-compat; prefer this for new code.)
///
/// # Safety
///
/// `document` must be a valid open document. `out_json` must be writable and
/// any returned string must be freed with `oxide_string_free`. If `error_out`
/// is non-null, it must be writable and any returned string must be freed with
/// `oxide_error_free`.
#[no_mangle]
pub unsafe extern "C" fn oxide_document_parse_json(
    document: *const OxideDocument,
    out_json: *mut *mut c_char,
    error_out: *mut *mut c_char,
) -> c_int {
    ffi_status(error_out, || {
        let doc = checked_doc(document)?;
        if out_json.is_null() {
            return Err("out_json pointer is null".into());
        }
        let parsed = oxide(doc.engine.parse_document(&ParseOptions::default()))?;
        unsafe {
            *out_json = into_c_string(parsed.to_json());
        }
        Ok(())
    })
}

/// Extracts structured key-value fields (invoice number/date/total, receipt
/// merchant/amount, form label→value pairs, line items) as JSON.
///
/// `doc_type` selects the document-type profile: pass a null pointer or one of
/// `"auto"`, `"invoice"`, `"receipt"`, `"form"`, `"generic"`. Null/empty/unknown
/// behaves as `"auto"` (auto-detect). Digital-born only — OCR is not wired
/// through the C ABI.
///
/// # Safety
///
/// `document` must be a valid open document. `doc_type` must be null or a valid
/// NUL-terminated C string. `out_json` must be writable and any returned string
/// must be freed with `oxide_string_free`. If `error_out` is non-null, it must
/// be writable and any returned string must be freed with `oxide_error_free`.
#[no_mangle]
pub unsafe extern "C" fn oxide_document_extract_fields_json(
    document: *const OxideDocument,
    doc_type: *const c_char,
    out_json: *mut *mut c_char,
    error_out: *mut *mut c_char,
) -> c_int {
    ffi_status(error_out, || {
        let doc = checked_doc(document)?;
        if out_json.is_null() {
            return Err("out_json pointer is null".into());
        }
        // A null or empty doc_type means auto-detect; an unrecognized value also
        // falls back to auto rather than erroring, matching the CLI/WASM surface.
        let doc_type = if doc_type.is_null() {
            None
        } else {
            let s = unsafe { std::ffi::CStr::from_ptr(doc_type) }
                .to_str()
                .map_err(|_| "doc_type is not valid UTF-8".to_string())?;
            DocType::parse(s)
        };
        let opts = ExtractOptions {
            doc_type,
            ..Default::default()
        };
        let fields = oxide(doc.engine.extract_fields(&opts))?;
        unsafe {
            *out_json = into_c_string(fields.to_json());
        }
        Ok(())
    })
}

/// Returns document metadata as JSON.
///
/// # Safety
///
/// `document` must be a valid open document. `out_json` must be writable and
/// any returned string must be freed with `oxide_string_free`. If `error_out`
/// is non-null, it must be writable and any returned string must be freed with
/// `oxide_error_free`.
#[no_mangle]
pub unsafe extern "C" fn oxide_document_info_json(
    document: *const OxideDocument,
    out_json: *mut *mut c_char,
    error_out: *mut *mut c_char,
) -> c_int {
    ffi_status(error_out, || {
        let doc = checked_doc(document)?;
        if out_json.is_null() {
            return Err("out_json pointer is null".into());
        }
        let info = oxide(doc.engine.document_info())?;
        let json = serde_json::to_string(&info).map_err(|err| err.to_string())?;
        unsafe {
            *out_json = into_c_string(json);
        }
        Ok(())
    })
}

/// Renders a page to PNG bytes.
///
/// # Safety
///
/// `document` must be a valid open document. `out_buffer` must be writable and
/// any returned buffer must be freed with `oxide_buffer_free`. If `error_out`
/// is non-null, it must be writable and any returned string must be freed with
/// `oxide_error_free`.
#[no_mangle]
pub unsafe extern "C" fn oxide_document_render_page_png(
    document: *const OxideDocument,
    page: usize,
    dpi: u32,
    out_buffer: *mut OxideBuffer,
    error_out: *mut *mut c_char,
) -> c_int {
    ffi_status(error_out, || {
        let doc = checked_doc(document)?;
        if out_buffer.is_null() {
            return Err("out_buffer pointer is null".into());
        }
        let png = oxide(doc.engine.render_page_png_fast(page, dpi))?;
        unsafe {
            *out_buffer = into_buffer(png);
        }
        Ok(())
    })
}

fn checked_doc<'a>(document: *const OxideDocument) -> Result<&'a OxideDocument, String> {
    if document.is_null() {
        Err("document pointer is null".to_string())
    } else {
        Ok(unsafe { &*document })
    }
}

fn ffi_status(error_out: *mut *mut c_char, f: impl FnOnce() -> Result<(), String>) -> c_int {
    clear_error(error_out);
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(())) => OXIDE_STATUS_OK,
        Ok(Err(err)) => {
            set_error(error_out, &err);
            OXIDE_STATUS_ERROR
        }
        Err(_) => {
            set_error(error_out, "panic inside oxide C API");
            OXIDE_STATUS_PANIC
        }
    }
}

fn oxide<T>(result: OxideResult<T>) -> Result<T, String> {
    result.map_err(|err| err.to_string())
}

fn into_c_string(value: String) -> *mut c_char {
    let clean = value.replace('\0', "\u{FFFD}");
    CString::new(clean).expect("nul bytes replaced").into_raw()
}

fn into_buffer(bytes: Vec<u8>) -> OxideBuffer {
    if bytes.is_empty() {
        return OxideBuffer::empty();
    }
    let mut bytes = bytes.into_boxed_slice();
    let out = OxideBuffer {
        data: bytes.as_mut_ptr(),
        len: bytes.len(),
    };
    std::mem::forget(bytes);
    out
}

fn set_error(error_out: *mut *mut c_char, message: &str) {
    if !error_out.is_null() {
        unsafe {
            *error_out = into_c_string(message.to_string());
        }
    }
}

fn clear_error(error_out: *mut *mut c_char) {
    if !error_out.is_null() {
        unsafe {
            *error_out = ptr::null_mut();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CStr;

    struct PdfBuilder {
        objects: Vec<Vec<u8>>,
    }

    impl PdfBuilder {
        fn new() -> Self {
            Self {
                objects: Vec::new(),
            }
        }

        fn add(&mut self, body: &str) {
            self.objects.push(body.as_bytes().to_vec());
        }

        fn add_stream(&mut self, stream: &[u8]) {
            let mut body = format!("<< /Length {} >>\nstream\n", stream.len()).into_bytes();
            body.extend_from_slice(stream);
            body.extend_from_slice(b"\nendstream");
            self.objects.push(body);
        }

        fn build(&self) -> Vec<u8> {
            let mut pdf = Vec::new();
            pdf.extend_from_slice(b"%PDF-1.7\n");
            let mut offsets = Vec::new();
            for (i, body) in self.objects.iter().enumerate() {
                offsets.push(pdf.len());
                pdf.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
                pdf.extend_from_slice(body);
                pdf.extend_from_slice(b"\nendobj\n");
            }
            let xref_start = pdf.len();
            pdf.extend_from_slice(format!("xref\n0 {}\n", offsets.len() + 1).as_bytes());
            pdf.extend_from_slice(b"0000000000 65535 f \n");
            for off in offsets {
                pdf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
            }
            pdf.extend_from_slice(
                format!(
                    "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF",
                    self.objects.len() + 1,
                    xref_start
                )
                .as_bytes(),
            );
            pdf
        }
    }

    fn sample_pdf() -> Vec<u8> {
        let mut b = PdfBuilder::new();
        b.add("<< /Type /Catalog /Pages 2 0 R >>");
        b.add("<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.add(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
             /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
        );
        b.add_stream(b"BT /F1 12 Tf 40 120 Td (Hello C API) Tj ET");
        b.add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
        b.build()
    }

    #[test]
    fn capi_open_count_extract_and_free() {
        let pdf = sample_pdf();
        let mut error = std::ptr::null_mut();
        let doc = unsafe { oxide_document_open_from_bytes(pdf.as_ptr(), pdf.len(), &mut error) };
        assert!(!doc.is_null());
        assert!(error.is_null());

        let mut count = 0usize;
        let status = unsafe { oxide_document_page_count(doc, &mut count, &mut error) };
        assert_eq!(status, OXIDE_STATUS_OK);
        assert_eq!(count, 1);

        let mut text = std::ptr::null_mut();
        let status = unsafe { oxide_document_extract_text(doc, 1, &mut text, &mut error) };
        assert_eq!(status, OXIDE_STATUS_OK);
        let extracted = unsafe { CStr::from_ptr(text) }
            .to_string_lossy()
            .into_owned();
        assert!(extracted.contains("Hello C API"));
        unsafe {
            oxide_string_free(text);
            oxide_document_free(doc);
        }
    }

    #[test]
    fn capi_parse_markdown_json_and_fields() {
        let pdf = sample_pdf();
        let mut error = std::ptr::null_mut();
        let doc = unsafe { oxide_document_open_from_bytes(pdf.as_ptr(), pdf.len(), &mut error) };
        assert!(!doc.is_null());

        // parse → markdown: the canonical parser output, containing the text.
        let mut md = std::ptr::null_mut();
        let status = unsafe { oxide_document_parse_markdown(doc, &mut md, &mut error) };
        assert_eq!(status, OXIDE_STATUS_OK);
        let markdown = unsafe { CStr::from_ptr(md) }.to_string_lossy().into_owned();
        assert!(markdown.contains("Hello C API"), "markdown was: {markdown}");
        unsafe { oxide_string_free(md) };

        // parse → canonical JSON: must carry the schema version and the text.
        let mut json = std::ptr::null_mut();
        let status = unsafe { oxide_document_parse_json(doc, &mut json, &mut error) };
        assert_eq!(status, OXIDE_STATUS_OK);
        let parsed = unsafe { CStr::from_ptr(json) }.to_string_lossy().into_owned();
        assert!(parsed.contains("schema_version"), "json was: {parsed}");
        assert!(parsed.contains("Hello C API"));
        unsafe { oxide_string_free(json) };

        // extract-fields → JSON: null doc_type means auto-detect; must succeed
        // and produce a well-formed payload (this doc has no fields, which is
        // fine — the call must not error and must include the schema version).
        let mut fields = std::ptr::null_mut();
        let status = unsafe {
            oxide_document_extract_fields_json(doc, std::ptr::null(), &mut fields, &mut error)
        };
        assert_eq!(status, OXIDE_STATUS_OK);
        let fields_json = unsafe { CStr::from_ptr(fields) }
            .to_string_lossy()
            .into_owned();
        assert!(fields_json.contains("schema_version"), "fields: {fields_json}");
        unsafe { oxide_string_free(fields) };

        unsafe { oxide_document_free(doc) };
    }

    #[test]
    fn capi_reports_null_document_error() {
        let mut count = 0usize;
        let mut error = std::ptr::null_mut();
        let status = unsafe { oxide_document_page_count(std::ptr::null(), &mut count, &mut error) };
        assert_eq!(status, OXIDE_STATUS_ERROR);
        assert!(!error.is_null());
        let message = unsafe { CStr::from_ptr(error) }
            .to_string_lossy()
            .into_owned();
        assert!(message.contains("document pointer is null"));
        unsafe {
            oxide_error_free(error);
        }
    }
}
