/*
 * Unified Query Algebra
 *
 * Copyright (c) 2023-2026 Cognica, Inc.
 */

#include "example.h"
#include <uqa/uqa_udf.h>
#include <ctype.h>

static uqa_status assign_result(uqa_udf_context context, uqa_value value) {
    uqa_status status = uqa_udf_context_set_result(context, value);
    uqa_status cleanup = uqa_value_destroy(&value);
    return status == UQA_OK ? cleanup : status;
}

static uqa_status UQA_CALL normalize_label(uqa_udf_context context,
    uqa_value_view const *arguments, uint32_t count, void *user_data) {
    (void)user_data;
    if (count != 1) return uqa_udf_context_fail(context, "normalize_label expects one text argument");
    uqa_string_view text = {NULL, 0};
    uqa_status status = uqa_value_get_text(arguments[0], &text);
    if (status != UQA_OK) return status;
    size_t start = 0, end = text.size;
    while (start < end && isspace((unsigned char)text.data[start])) ++start;
    while (end > start && isspace((unsigned char)text.data[end - 1])) --end;
    char *label = (char *)malloc(end - start + 1);
    if (label == NULL) return UQA_ERROR_OUT_OF_MEMORY;
    for (size_t i = start; i < end; ++i) {
        unsigned char character = (unsigned char)text.data[i];
        label[i - start] = character == ' ' ? '-' : (char)tolower(character);
    }
    label[end - start] = '\0';
    uqa_value value = NULL;
    status = uqa_value_create_text(label, &value);
    free(label);
    return status == UQA_OK ? assign_result(context, value) : status;
}

typedef struct repeat_scan {
    uqa_value label;
    int64_t position;
    int64_t count;
} repeat_scan;

static uqa_status UQA_CALL repeat_create(uqa_value_view const *arguments,
    uint32_t count, void *user_data, void **out_scan) {
    (void)user_data;
    if (count != 2) return uqa_callback_fail(UQA_ERROR_TYPE,
        "repeat_rows expects text and a non-negative integer");
    uqa_string_view label = {NULL, 0};
    uqa_status status = uqa_value_get_text(arguments[0], &label);
    if (status != UQA_OK) return status;
    int64_t times = 0;
    status = uqa_value_get_int64(arguments[1], &times);
    if (status != UQA_OK) return status;
    if (times < 0) return uqa_callback_fail(UQA_ERROR_RANGE,
        "repeat_rows expects a non-negative count");
    repeat_scan *scan = (repeat_scan *)calloc(1, sizeof(*scan));
    if (scan == NULL) return UQA_ERROR_OUT_OF_MEMORY;
    status = uqa_value_create_text_n(label.data, label.size, &scan->label);
    if (status != UQA_OK) { free(scan); return status; }
    scan->count = times;
    *out_scan = scan;
    return UQA_OK;
}

static uqa_status UQA_CALL repeat_next(uqa_udf_row_sink sink, void *state, void *user_data) {
    (void)user_data;
    repeat_scan *scan = (repeat_scan *)state;
    if (scan->position == scan->count) return UQA_DONE;
    uqa_value index = NULL;
    uqa_status status = uqa_value_create_int64(scan->position, &index);
    if (status != UQA_OK) return status;
    uqa_value_view row[] = {scan->label, index};
    status = uqa_udf_row_sink_emit(sink, row, 2);
    uqa_status cleanup = uqa_value_destroy(&index);
    if (status != UQA_OK) return status;
    ++scan->position;
    return cleanup;
}

static uqa_status UQA_CALL repeat_destroy(void *state, void *user_data) {
    (void)user_data;
    repeat_scan *scan = (repeat_scan *)state;
    uqa_status status = uqa_value_destroy(&scan->label);
    free(scan);
    return status;
}

static uqa_status UQA_CALL sum_create(void *user_data, void **out_state) {
    (void)user_data;
    *out_state = calloc(1, sizeof(int64_t));
    return *out_state == NULL ? UQA_ERROR_OUT_OF_MEMORY : UQA_OK;
}

static uqa_status UQA_CALL sum_observe(void *state, uqa_value_view const *arguments,
    uint32_t count, void *user_data) {
    (void)user_data;
    if (count != 1) return uqa_callback_fail(UQA_ERROR_TYPE,
        "sum_squares expects one integer argument");
    uint8_t is_null = 0;
    uqa_status status = uqa_value_get_is_null(arguments[0], &is_null);
    if (status != UQA_OK || is_null) return status;
    int64_t value = 0;
    status = uqa_value_get_int64(arguments[0], &value);
    if (status != UQA_OK) return status;
    if (value > INT64_C(3037000499) || value < -INT64_C(3037000499))
        return uqa_callback_fail(UQA_ERROR_RANGE, "sum_squares multiplication overflow");
    int64_t square = value * value;
    int64_t *total = (int64_t *)state;
    if (*total > INT64_MAX - square)
        return uqa_callback_fail(UQA_ERROR_RANGE, "sum_squares addition overflow");
    *total += square;
    return UQA_OK;
}

static uqa_status UQA_CALL sum_finish(uqa_udf_context context, void *state, void *user_data) {
    (void)user_data;
    uqa_value value = NULL;
    uqa_status status = uqa_value_create_int64(*(int64_t *)state, &value);
    return status == UQA_OK ? assign_result(context, value) : status;
}

static uqa_status UQA_CALL sum_destroy(void *state, void *user_data) {
    (void)user_data;
    free(state);
    return UQA_OK;
}

int main(int argc, char **argv) {
    uqa_engine engine = example_open(example_arguments(argc, argv, 0));
    example_execute(engine,
        "CREATE TABLE samples (grp TEXT, label TEXT, value INTEGER);"
        "INSERT INTO samples VALUES ('a', ' SQL Manual ', 1), ('a', 'Node JS', 2),"
        "('b', 'Browser WASM', 3)");
    uqa_udf_options options = NULL;
    CHECK(uqa_udf_options_create(&options));
    CHECK(uqa_udf_options_set_may_mutate_engine(options, 0));
    CHECK(uqa_udf_options_set_volatility(options, UQA_VOLATILITY_IMMUTABLE));
    uqa_udf scalar = NULL, table = NULL, aggregate = NULL;
    CHECK(uqa_udf_register_scalar(engine, "normalize_label", options,
        normalize_label, NULL, NULL, &scalar));
    const char *columns[] = {"label", "idx"};
    CHECK(uqa_udf_register_table(engine, "repeat_rows", options, columns, 2,
        repeat_create, repeat_next, repeat_destroy, NULL, NULL, &table));
    CHECK(uqa_udf_register_aggregate(engine, "sum_squares", options,
        sum_create, sum_observe, sum_finish, sum_destroy, NULL, NULL, &aggregate));
    CHECK(uqa_udf_options_destroy(&options));
    uqa_result result = example_query(engine,
        "SELECT normalize_label(label) FROM samples ORDER BY value");
    REQUIRE(example_rows(result) == 3);
    example_text(result, 0, 0, "sql-manual");
    example_text(result, 1, 0, "node-js");
    example_text(result, 2, 0, "browser-wasm");
    CHECK(uqa_result_destroy(&result));
    result = example_query(engine,
        "SELECT label, idx FROM repeat_rows('row', 3) AS r(label, idx) ORDER BY idx");
    REQUIRE(example_rows(result) == 3);
    for (uint64_t i = 0; i < 3; ++i) {
        example_text(result, i, 0, "row");
        REQUIRE(example_integer(result, i, 1) == (int64_t)i);
    }
    CHECK(uqa_result_destroy(&result));
    result = example_query(engine,
        "SELECT grp, sum_squares(value) FROM samples GROUP BY grp ORDER BY grp");
    REQUIRE(example_rows(result) == 2);
    example_text(result, 0, 0, "a");
    example_text(result, 1, 0, "b");
    REQUIRE(example_integer(result, 0, 1) == 5);
    REQUIRE(example_integer(result, 1, 1) == 9);
    CHECK(uqa_result_destroy(&result));
    CHECK(uqa_udf_unregister(&aggregate));
    CHECK(uqa_udf_unregister(&table));
    CHECK(uqa_udf_unregister(&scalar));
    CHECK(uqa_engine_close(&engine));
    puts("C extensibility example passed");
    return EXIT_SUCCESS;
}
