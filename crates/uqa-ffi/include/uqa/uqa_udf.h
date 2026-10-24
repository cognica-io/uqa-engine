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

#define UQA_VOLATILITY_IMMUTABLE UINT32_C(0)
#define UQA_VOLATILITY_STABLE    UINT32_C(1)
#define UQA_VOLATILITY_VOLATILE  UINT32_C(2)

/* Defaults: VOLATILE and may_mutate_engine = 1. Setters validate each
 * property; registration validates the complete combination and copies it.
 * Invalid setters preserve the previous value. */
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

/* A successful create transfers scan ownership even when *out_scan is NULL.
 * A failed create remains responsible for its own partial allocations. */
typedef uqa_status (UQA_CALL *uqa_udf_create_scan_fn)(
    uqa_value_view const *arguments, uint32_t count, void *user_data,
    void **out_scan);
typedef uqa_status (UQA_CALL *uqa_udf_next_fn)(
    uqa_udf_row_sink sink, void *scan, void *user_data);
typedef uqa_status (UQA_CALL *uqa_udf_destroy_scan_fn)(
    void *scan, void *user_data);

typedef uqa_status (UQA_CALL *uqa_udf_create_state_fn)(
    void *user_data, void **out_state);
typedef uqa_status (UQA_CALL *uqa_udf_observe_fn)(
    void *state, uqa_value_view const *arguments, uint32_t count, void *user_data);
typedef uqa_status (UQA_CALL *uqa_udf_finish_fn)(
    uqa_udf_context context, void *state, void *user_data);
typedef uqa_status (UQA_CALL *uqa_udf_destroy_state_fn)(
    void *state, void *user_data);

UQA_API uqa_status UQA_CALL uqa_udf_register_scalar(
    uqa_engine engine, const char *name,
    uqa_udf_options options, uqa_udf_scalar_fn function,
    void *user_data, uqa_user_data_destroy_fn destroy_user_data,
    uqa_udf *out);
UQA_API uqa_status UQA_CALL uqa_udf_register_table(
    uqa_engine engine, const char *name, uqa_udf_options options,
    const char *const *columns, uint32_t column_count,
    uqa_udf_create_scan_fn create_scan, uqa_udf_next_fn next,
    uqa_udf_destroy_scan_fn destroy_scan,
    void *user_data, uqa_user_data_destroy_fn destroy_user_data, uqa_udf *out);
UQA_API uqa_status UQA_CALL uqa_udf_register_aggregate(
    uqa_engine engine, const char *name, uqa_udf_options options,
    uqa_udf_create_state_fn create_state, uqa_udf_observe_fn observe,
    uqa_udf_finish_fn finish, uqa_udf_destroy_state_fn destroy_state,
    void *user_data, uqa_user_data_destroy_fn destroy_user_data, uqa_udf *out);

/* These objects and argument views are borrowed for their active invocation.
 * Argument types follow native runtime carriers/physical OIDs, not the lost
 * expression declaration or modifiers; unknown NULL has no type descriptor.
 * Exactly one successful result assignment is required; explicit SQL NULL is
 * a value. Before assignment get_result returns UQA_DONE and a NULL view.
 * Failed/duplicate assignments preserve the previous result. */
UQA_API uqa_status UQA_CALL uqa_udf_context_get_result(
    uqa_udf_context context, uqa_value_view *out);
UQA_API uqa_status UQA_CALL uqa_udf_context_set_result(
    uqa_udf_context context, uqa_value_view value);
UQA_API uqa_status UQA_CALL uqa_udf_context_fail(
    uqa_udf_context context, const char *message);

/* next returns UQA_OK after emitting one row, or UQA_DONE without a row.
 * emit copies the values before returning; wrong width and a second row fail.
 * All other callbacks return UQA_OK or a negative failure status.
 * Every successful create has one destroy call, including on early stop or
 * failure. finish reads aggregate state without consuming it. Destruction
 * consumes ownership even when it fails; it is never retried. */
UQA_API uqa_status UQA_CALL uqa_udf_row_sink_emit(
    uqa_udf_row_sink sink, uqa_value_view const *values, uint32_t count);

/* Register copies options and table column names (UTF-8 C strings, at least
 * one column). Names and order are fixed for the registration, including
 * empty and correlated sources. Failure leaves user_data with the caller. Shared
 * user_data can be invoked concurrently by sibling sessions. Per-scan/group
 * calls are serialized but may move between workers. Engine reentry is
 * prohibited from every callback, including destruction; value/result and
 * callback-diagnostic helpers are allowed. Returning the most recent helper
 * failure status preserves its diagnostic through later successful helpers.
 * Native SQL errors retain their SQLSTATE. No exception/longjmp may escape.
 *
 * A handle keeps the shared registry, not its creating session. Unregister
 * returns UQA_ERROR_BUSY with the handle intact while execution retains it.
 * Success retires only that registration, destroys user_data exactly once and
 * clears the handle even when the destructor returns an error. */
UQA_API uqa_status UQA_CALL uqa_udf_unregister(uqa_udf *function);

#ifdef __cplusplus
}
#endif

#endif /* UQA_UQA_UDF_H */
