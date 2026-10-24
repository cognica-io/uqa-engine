/*
 * Unified Query Algebra
 *
 * Copyright (c) 2023-2026 Cognica, Inc.
 */

#ifndef UQA_UQA_UDF_H
#define UQA_UQA_UDF_H

#include <uqa/uqa.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct uqa_udf_t *uqa_udf;
typedef struct uqa_udf_context_t *uqa_udf_context;
typedef struct uqa_udf_row_sink_t *uqa_udf_row_sink;
typedef struct uqa_udf_options_t *uqa_udf_options;

UQA_API uqa_status UQA_CALL uqa_udf_options_create(uqa_udf_options *out);
UQA_API uqa_status UQA_CALL uqa_udf_options_get_volatility(
    uqa_udf_options options, uint32_t *out);
UQA_API uqa_status UQA_CALL uqa_udf_options_set_volatility(
    uqa_udf_options options, uint32_t volatility);
UQA_API uqa_status UQA_CALL uqa_udf_options_get_may_mutate_engine(
    uqa_udf_options options, uint8_t *out);
UQA_API uqa_status UQA_CALL uqa_udf_options_set_may_mutate_engine(
    uqa_udf_options options, uint8_t may_mutate_engine);
UQA_API uqa_status UQA_CALL uqa_udf_options_destroy(uqa_udf_options *options);

typedef uqa_status (UQA_CALL *uqa_udf_scalar_fn)(
    uqa_udf_context context,
    uqa_value_view const *arguments, uint32_t count,
    void *user_data);

UQA_API uqa_status UQA_CALL uqa_udf_register_scalar(
    uqa_engine engine, const char *name,
    uqa_udf_options options, uqa_udf_scalar_fn function,
    void *user_data, uqa_user_data_destroy_fn destroy_user_data,
    uqa_udf *out);
UQA_API uqa_status UQA_CALL uqa_udf_context_get_result(
    uqa_udf_context context, uqa_value_view *out);
UQA_API uqa_status UQA_CALL uqa_udf_context_set_result(
    uqa_udf_context context, uqa_value_view value);
UQA_API uqa_status UQA_CALL uqa_udf_context_fail(
    uqa_udf_context context, const char *message);
UQA_API uqa_status UQA_CALL uqa_udf_unregister(uqa_udf *function);

#ifdef __cplusplus
}
#endif

#endif /* UQA_UQA_UDF_H */
