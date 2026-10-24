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

struct ArrowSchema;
struct ArrowArray;

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
