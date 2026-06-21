#include "oxide.h"

#include <stdio.h>
#include <stdlib.h>

static unsigned char *read_file(const char *path, size_t *len) {
  FILE *file = fopen(path, "rb");
  if (!file) {
    return NULL;
  }
  fseek(file, 0, SEEK_END);
  long size = ftell(file);
  if (size < 0) {
    fclose(file);
    return NULL;
  }
  fseek(file, 0, SEEK_SET);
  unsigned char *data = (unsigned char *)malloc((size_t)size);
  if (!data) {
    fclose(file);
    return NULL;
  }
  if (fread(data, 1, (size_t)size, file) != (size_t)size) {
    free(data);
    fclose(file);
    return NULL;
  }
  fclose(file);
  *len = (size_t)size;
  return data;
}

int main(int argc, char **argv) {
  if (argc != 2) {
    fprintf(stderr, "usage: %s input.pdf\n", argv[0]);
    return 2;
  }

  size_t len = 0;
  unsigned char *bytes = read_file(argv[1], &len);
  if (!bytes) {
    fprintf(stderr, "could not read %s\n", argv[1]);
    return 2;
  }

  char *error = NULL;
  OxideDocument *doc = oxide_document_open_from_bytes(bytes, len, &error);
  free(bytes);
  if (!doc) {
    fprintf(stderr, "open failed: %s\n", error ? error : "unknown error");
    oxide_error_free(error);
    return 1;
  }

  char *text = NULL;
  int status = oxide_document_extract_text(doc, 1, &text, &error);
  if (status != OXIDE_STATUS_OK) {
    fprintf(stderr, "extract failed: %s\n", error ? error : "unknown error");
    oxide_error_free(error);
    oxide_document_free(doc);
    return 1;
  }

  puts(text);
  oxide_string_free(text);
  oxide_document_free(doc);
  return 0;
}
