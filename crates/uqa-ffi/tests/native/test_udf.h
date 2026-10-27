/*
 * Unified Query Algebra
 *
 * Copyright (c) 2023-2026 Cognica, Inc.
 */

#ifndef UQA_TEST_UDF_H
#define UQA_TEST_UDF_H

#include <uqa/uqa_udf.h>
#include "test_database.h"

static inline void udf_checked(uqa_status status, int line) {
    if (status != UQA_OK) {
        uqa_error error = NULL;
        uqa_string_view message = {NULL, 0}, detail = {NULL, 0};
        (void)uqa_error_get_last(&error);
        if (error != NULL) {
            (void)uqa_error_get_message(error, &message);
            (void)uqa_error_get_detail(error, &detail);
        }
        fprintf(stderr, "line %d: status %d: %.*s\n%.*s\n", line, (int)status,
                (int)message.size, message.data == NULL ? "" : message.data,
                (int)detail.size, detail.data == NULL ? "" : detail.data);
        abort();
    }
}
#define CHECK(call) udf_checked((call), __LINE__)

static inline int udf_equal(uqa_string_view view, const char *text) {
    return view.size == strlen(text) && (view.size == 0 || memcmp(view.data, text, view.size) == 0);
}

static inline void udf_sqlstate(const char *expected) {
    uqa_error error = NULL;
    uqa_string_view state = {NULL, 0};
    CHECK(uqa_error_get_last(&error));
    CHECK(uqa_error_get_sqlstate(error, &state));
    assert(udf_equal(state, expected));
}

static inline void udf_failure(const char *message, uqa_status cause_status) {
    uqa_error error = NULL, cause = NULL;
    uqa_string_view text = {NULL, 0}, sqlstate = {NULL, 0};
    uqa_status status = 0;
    CHECK(uqa_error_get_last(&error));
    CHECK(uqa_error_get_message(error, &text));
    assert(udf_equal(text, message));
    CHECK(uqa_error_get_sqlstate(error, &sqlstate));
    assert(udf_equal(sqlstate, "38000"));
    CHECK(uqa_error_get_cause(error, &cause));
    CHECK(uqa_error_get_status(cause, &status));
    assert(status == cause_status);
}

static inline int64_t udf_result_int(uqa_result result, uint64_t row, uint32_t column) {
    uqa_value_view value = NULL;
    int64_t number = 0;
    CHECK(uqa_result_get_value(result, row, column, &value));
    CHECK(uqa_value_get_int64(value, &number));
    return number;
}

static inline uqa_status udf_set_int(uqa_udf_context context, int64_t number) {
    uqa_value value = NULL;
    CHECK(uqa_value_create_int64(number, &value));
    uqa_status status = uqa_udf_context_set_result(context, value);
    CHECK(uqa_value_destroy(&value));
    return status;
}

#endif
