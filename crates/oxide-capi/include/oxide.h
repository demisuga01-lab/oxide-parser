#ifndef OXIDE_H
#define OXIDE_H

#include <stddef.h>
#include <stdint.h>

#ifdef _WIN32
#  ifdef OXIDE_BUILDING_DLL
#    define OXIDE_API __declspec(dllexport)
#  else
#    define OXIDE_API __declspec(dllimport)
#  endif
#else
#  define OXIDE_API
#endif

#ifdef __cplusplus
extern "C" {
#endif

enum {
  OXIDE_STATUS_OK = 0,
  OXIDE_STATUS_NULL = 1,
  OXIDE_STATUS_ERROR = 2,
  OXIDE_STATUS_PANIC = 3
};

typedef struct OxideDocument OxideDocument;

typedef struct OxideBuffer {
  uint8_t *data;
  size_t len;
} OxideBuffer;

OXIDE_API OxideDocument *oxide_document_open_from_bytes(
    const uint8_t *data,
    size_t len,
    char **error_out);

OXIDE_API void oxide_document_free(OxideDocument *document);

OXIDE_API void oxide_string_free(char *value);

OXIDE_API void oxide_error_free(char *value);

OXIDE_API void oxide_buffer_free(OxideBuffer buffer);

OXIDE_API int oxide_document_page_count(
    const OxideDocument *document,
    size_t *out_count,
    char **error_out);

OXIDE_API int oxide_document_extract_text(
    const OxideDocument *document,
    size_t page,
    char **out_text,
    char **error_out);

OXIDE_API int oxide_document_parse_markdown(
    const OxideDocument *document,
    char **out_markdown,
    char **error_out);

OXIDE_API int oxide_document_parse_json(
    const OxideDocument *document,
    char **out_json,
    char **error_out);

OXIDE_API int oxide_document_extract_fields_json(
    const OxideDocument *document,
    const char *doc_type,
    char **out_json,
    char **error_out);

OXIDE_API int oxide_document_extract_semantic_json(
    const OxideDocument *document,
    char **out_json,
    char **error_out);

OXIDE_API int oxide_document_info_json(
    const OxideDocument *document,
    char **out_json,
    char **error_out);

OXIDE_API int oxide_document_render_page_png(
    const OxideDocument *document,
    size_t page,
    uint32_t dpi,
    OxideBuffer *out_buffer,
    char **error_out);

#ifdef __cplusplus
}
#endif

#endif
