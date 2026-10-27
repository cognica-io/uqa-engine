/*
 * Unified Query Algebra
 *
 * Copyright (c) 2023-2026 Cognica, Inc.
 */

#ifndef UQA_UQA_ARROW_H
#define UQA_UQA_ARROW_H

#include <uqa/uqa.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Apache Arrow C Data Interface, Apache License 2.0.
 * https://arrow.apache.org/docs/format/CDataInterface.html
 * The standard names and guard permit inclusion alongside other Arrow headers.
 */
#ifndef ARROW_C_DATA_INTERFACE
#define ARROW_C_DATA_INTERFACE
#define ARROW_FLAG_DICTIONARY_ORDERED 1
#define ARROW_FLAG_NULLABLE 2
#define ARROW_FLAG_MAP_KEYS_SORTED 4

struct ArrowSchema {
    const char *format;
    const char *name;
    const char *metadata;
    int64_t flags;
    int64_t n_children;
    struct ArrowSchema **children;
    struct ArrowSchema *dictionary;
    void (*release)(struct ArrowSchema *);
    void *private_data;
};

struct ArrowArray {
    int64_t length;
    int64_t null_count;
    int64_t offset;
    int64_t n_buffers;
    int64_t n_children;
    const void **buffers;
    struct ArrowArray **children;
    struct ArrowArray *dictionary;
    void (*release)(struct ArrowArray *);
    void *private_data;
};
#endif /* ARROW_C_DATA_INTERFACE */

#define UQA_ARROW_LOSSLESS UINT32_C(0)
#define UQA_ARROW_SQL_TEXT UINT32_C(1)

/* Both outputs require release == NULL. Successful outputs have independent
 * lifetimes, including after moving either struct or closing the Engine.
 * Release each once before unloading the library. Failure leaves outputs empty.
 * Builds without arrow-export retain these symbols and report UNSUPPORTED.
 */

UQA_API uqa_status UQA_CALL uqa_result_export_arrow(
    uqa_result result, uint32_t policy, struct ArrowSchema *out_schema,
    struct ArrowArray *out_array);
UQA_API uqa_status UQA_CALL uqa_cursor_fetch_arrow(
    uqa_cursor cursor, uint32_t max_rows, uint32_t policy,
    struct ArrowSchema *out_schema, struct ArrowArray *out_array);

#ifdef __cplusplus
}
#endif

#endif /* UQA_UQA_ARROW_H */
