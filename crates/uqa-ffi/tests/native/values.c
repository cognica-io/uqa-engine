/*
 * Unified Query Algebra
 *
 * Copyright (c) 2023-2026 Cognica, Inc.
 */

#include <uqa/uqa.h>
#include "test_database.h"
#include <assert.h>
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* SQL values, explicit parameter types and SQLSTATEs were independently
 * captured from PostgreSQL 18.4 in values.pg18.json. */

static void checked(uqa_status status, int line) {
    if (status != UQA_OK) {
        uqa_error error = NULL;
        uqa_string_view message = {NULL, 0};
        if (uqa_error_get_last(&error) == UQA_OK) {
            (void)uqa_error_get_message(error, &message);
        }
        fprintf(stderr, "line %d: status %d: %.*s\n", line, (int)status,
                (int)message.size, message.data == NULL ? "" : message.data);
        abort();
    }
}
#define CHECK(call) checked((call), __LINE__)

static int equal(uqa_string_view value, const char *text) {
    size_t size = strlen(text);
    return value.size == size && (size == 0 || memcmp(value.data, text, size) == 0);
}

static void sql_error(uqa_status status, const char *state) {
    uqa_error error = NULL;
    uqa_string_view actual = {NULL, 0};
    assert(status == UQA_ERROR_SQL);
    CHECK(uqa_error_get_last(&error));
    CHECK(uqa_error_get_sqlstate(error, &actual));
    assert(equal(actual, state));
}

static void kind(uqa_value_view value, uqa_type_kind expected) {
    uqa_type type = NULL;
    uqa_type_kind actual = UQA_TYPE_UNKNOWN;
    CHECK(uqa_value_get_type(value, &type));
    CHECK(uqa_type_get_kind(type, &actual));
    assert(actual == expected);
}

static uqa_value_view cell(uqa_result result, uint32_t column) {
    uqa_value_view value = NULL;
    CHECK(uqa_result_get_value(result, 0, column, &value));
    return value;
}

static void integer_widths(uqa_engine engine, uqa_result result) {
    uqa_value values[3] = {NULL, NULL, NULL};
    uqa_value copy = NULL;
    int16_t i16 = 0;
    int32_t i32 = 0;
    int64_t i64 = 0;
    CHECK(uqa_value_create_int16(INT16_MAX, &values[0]));
    CHECK(uqa_value_create_int32(INT32_MAX, &values[1]));
    CHECK(uqa_value_create_int64(INT64_MAX, &values[2]));
    CHECK(uqa_value_get_int16(values[0], &i16));
    CHECK(uqa_value_get_int32(values[0], &i32));
    CHECK(uqa_value_get_int64(values[0], &i64));
    assert(i16 == INT16_MAX && i32 == INT16_MAX && i64 == INT16_MAX);
    assert(uqa_value_get_int16(values[1], &i16) == UQA_ERROR_TYPE && i16 == 0);
    CHECK(uqa_value_get_int32(values[1], &i32));
    assert(i32 == INT32_MAX);
    assert(uqa_value_get_int32(values[2], &i32) == UQA_ERROR_TYPE && i32 == 0);
    CHECK(uqa_value_get_int64(values[2], &i64));
    assert(i64 == INT64_MAX);
    CHECK(uqa_value_create_copy(values[0], &copy));
    CHECK(uqa_value_destroy(&values[0]));
    values[0] = copy;
    copy = NULL;
    for (uint32_t index = 0; index < 3; ++index) {
        uqa_value_view params[] = {values[index]};
        uqa_type_kind expected[] = {UQA_TYPE_INT16, UQA_TYPE_INT32, UQA_TYPE_INT64};
        const char *names[] = {"smallint", "integer", "bigint"};
        uqa_string_view text = {NULL, 0};
        kind(values[index], expected[index]);
        CHECK(uqa_engine_execute_params(engine, "SELECT $1, pg_typeof($1)::text", params, 1, result));
        kind(cell(result, 0), expected[index]);
        CHECK(uqa_value_get_text(cell(result, 1), &text));
        assert(equal(text, names[index]));
        sql_error(uqa_engine_execute_params(engine, "SELECT $1 + $1", params, 1, result), "22003");
        CHECK(uqa_value_destroy(&values[index]));
    }
}

static void floats(uqa_engine engine, uqa_result result) {
    uqa_value single = NULL;
    uqa_value wide = NULL;
    float f32 = 0.0f;
    double f64 = 0.0;
    CHECK(uqa_value_create_float32(0.1f, &single));
    CHECK(uqa_value_create_float64(0.1, &wide));
    kind(single, UQA_TYPE_FLOAT32);
    kind(wide, UQA_TYPE_FLOAT64);
    CHECK(uqa_value_get_float32(single, &f32));
    CHECK(uqa_value_get_float64(single, &f64));
    assert(f32 == 0.1f && f64 == (double)0.1f);
    assert(uqa_value_get_float32(wide, &f32) == UQA_ERROR_TYPE && f32 == 0.0f);
    {
        uqa_value_view params[] = {single};
        CHECK(uqa_engine_execute_params(engine, "SELECT $1, $1 + $1", params, 1, result));
        kind(cell(result, 0), UQA_TYPE_FLOAT32);
        kind(cell(result, 1), UQA_TYPE_FLOAT32);
        CHECK(uqa_value_get_float32(cell(result, 1), &f32));
        assert(f32 == 0.2f);
    }
    CHECK(uqa_value_destroy(&single));
    CHECK(uqa_value_destroy(&wide));
    {
        const float specials[] = {NAN, INFINITY, -INFINITY, -0.0f};
        for (size_t index = 0; index < sizeof(specials) / sizeof(specials[0]); ++index) {
            CHECK(uqa_value_create_float32(specials[index], &single));
            CHECK(uqa_value_get_float32(single, &f32));
            if (index == 0) assert(isnan(f32));
            else assert(f32 == specials[index] && !!signbit(f32) == !!signbit(specials[index]));
            CHECK(uqa_value_destroy(&single));
        }
    }
}

static void numeric(uqa_engine engine, uqa_result result) {
    const char *exact = "123456789012345678901234567890.0012300";
    uqa_value number = NULL;
    uqa_value copied = NULL;
    uqa_string_view text = {NULL, 0};
    uqa_type type = NULL;
    uint32_t precision = 99;
    uint32_t display_scale = 99;
    int32_t scale = 99;
    int64_t integer = 99;
    CHECK(uqa_value_create_numeric(exact, &number));
    kind(number, UQA_TYPE_NUMERIC);
    CHECK(uqa_value_get_numeric(number, &text));
    assert(equal(text, exact));
    CHECK(uqa_value_get_numeric_scale(number, &display_scale));
    assert(display_scale == 7);
    CHECK(uqa_value_get_type(number, &type));
    assert(uqa_type_get_numeric_precision(type, &precision) == UQA_DONE && precision == 0);
    assert(uqa_type_get_numeric_scale(type, &scale) == UQA_DONE && scale == 0);
    assert(uqa_value_get_int64(number, &integer) == UQA_ERROR_TYPE && integer == 0);
    assert(uqa_value_get_text(number, &text) == UQA_ERROR_TYPE && text.data == NULL);
    {
        uqa_value_view params[] = {number};
        CHECK(uqa_engine_execute_params(engine, "SELECT $1, $1 + 0.0000001", params, 1, result));
        CHECK(uqa_value_get_numeric(cell(result, 0), &text));
        assert(equal(text, exact));
        CHECK(uqa_value_get_numeric(cell(result, 1), &text));
        assert(equal(text, "123456789012345678901234567890.0012301"));
        CHECK(uqa_value_create_copy(cell(result, 0), &copied));
    }
    CHECK(uqa_value_destroy(&number));
    CHECK(uqa_engine_execute(engine, "SELECT 12.3456::numeric(5,2), 12345::numeric(2,-3)", result));
    CHECK(uqa_value_get_numeric(copied, &text));
    assert(equal(text, exact));
    CHECK(uqa_value_destroy(&copied));
    CHECK(uqa_value_get_numeric(cell(result, 0), &text));
    assert(equal(text, "12.35"));
    CHECK(uqa_value_get_type(cell(result, 0), &type));
    CHECK(uqa_type_get_numeric_precision(type, &precision));
    CHECK(uqa_type_get_numeric_scale(type, &scale));
    assert(precision == 5 && scale == 2);
    CHECK(uqa_value_get_numeric(cell(result, 1), &text));
    assert(equal(text, "12000"));
    CHECK(uqa_value_get_type(cell(result, 1), &type));
    CHECK(uqa_type_get_numeric_scale(type, &scale));
    assert(scale == -3);
    {
        const char *specials[] = {"NaN", "Infinity", "-Infinity"};
        for (size_t index = 0; index < 3; ++index) {
            CHECK(uqa_value_create_numeric(specials[index], &number));
            CHECK(uqa_value_get_numeric(number, &text));
            assert(equal(text, specials[index]));
            assert(uqa_value_get_numeric_scale(number, &display_scale) == UQA_DONE && display_scale == 0);
            CHECK(uqa_value_destroy(&number));
        }
    }
    CHECK(uqa_value_create_numeric_n("-1.2300 trailing", 7, &number));
    CHECK(uqa_value_get_numeric(number, &text));
    assert(equal(text, "-1.2300"));
    CHECK(uqa_value_destroy(&number));
    sql_error(uqa_value_create_numeric("1.2.3", &number), "22P02");
    assert(number == NULL);
    assert(uqa_value_create_numeric_n("1\0", 2, &number) == UQA_ERROR_INVALID_ARGUMENT);
}

static void json_values(uqa_engine engine, uqa_result result) {
    const char *input = "{ \"b\": 2, \"a\": 1, \"a\": 3 }";
    uqa_value json = NULL;
    uqa_value jsonb = NULL;
    uqa_string_view text = {NULL, 0};
    CHECK(uqa_value_create_json(input, &json));
    CHECK(uqa_value_create_jsonb_n(input, strlen(input), &jsonb));
    kind(json, UQA_TYPE_JSON);
    kind(jsonb, UQA_TYPE_JSONB);
    CHECK(uqa_value_get_json(json, &text));
    assert(equal(text, input));
    CHECK(uqa_value_get_jsonb(jsonb, &text));
    assert(equal(text, "{\"a\": 3, \"b\": 2}"));
    assert(uqa_value_get_jsonb(json, &text) == UQA_ERROR_TYPE && text.data == NULL);
    assert(uqa_value_get_json(jsonb, &text) == UQA_ERROR_TYPE && text.data == NULL);
    assert(uqa_value_get_text(json, &text) == UQA_ERROR_TYPE && text.data == NULL);
    {
        uqa_value_view params[] = {json, jsonb};
        CHECK(uqa_engine_execute_params(engine, "SELECT $1, $2, $1 ->> 'a', $2 ->> 'a'", params, 2, result));
        CHECK(uqa_value_get_json(cell(result, 0), &text));
        assert(equal(text, input));
        CHECK(uqa_value_get_jsonb(cell(result, 1), &text));
        assert(equal(text, "{\"a\": 3, \"b\": 2}"));
        CHECK(uqa_value_get_text(cell(result, 2), &text));
        assert(equal(text, "3"));
        CHECK(uqa_value_get_text(cell(result, 3), &text));
        assert(equal(text, "3"));
    }
    CHECK(uqa_value_destroy(&json));
    CHECK(uqa_value_destroy(&jsonb));
    sql_error(uqa_value_create_json("[1,]", &json), "22P02");
    sql_error(uqa_value_create_jsonb("{", &jsonb), "22P02");
    assert(json == NULL && jsonb == NULL);
    CHECK(uqa_value_create_json_n("null trailing", 4, &json));
    {
        uint8_t is_null = 1;
        CHECK(uqa_value_get_is_null(json, &is_null));
        assert(is_null == 0);
        CHECK(uqa_value_get_json(json, &text));
        assert(equal(text, "null"));
    }
    CHECK(uqa_value_destroy(&json));
}

static void nulls_text_and_vectors(uqa_engine engine, uqa_result result) {
    uqa_value value = NULL;
    uqa_value copy = NULL;
    uqa_type type = NULL;
    int16_t integer = 99;
    uqa_string_view text = {NULL, 0};
    uint8_t is_null = 0;
    CHECK(uqa_value_create_null(&value));
    assert(uqa_value_get_type(value, &type) == UQA_DONE && type == NULL);
    assert(uqa_value_get_int16(value, &integer) == UQA_ERROR_NULL && integer == 0);
    CHECK(uqa_value_destroy(&value));
    CHECK(uqa_engine_execute(engine, "SELECT NULL::smallint", result));
    CHECK(uqa_value_create_copy(cell(result, 0), &copy));
    kind(copy, UQA_TYPE_INT16);
    CHECK(uqa_value_get_is_null(copy, &is_null));
    assert(is_null == 1);
    CHECK(uqa_value_destroy(&copy));
    CHECK(uqa_value_create_text("42", &value));
    kind(value, UQA_TYPE_TEXT);
    {
        uqa_value_view params[] = {value};
        sql_error(uqa_engine_execute_params(engine, "SELECT $1 + 1", params, 1, result), "42883");
        CHECK(uqa_engine_execute_params(engine, "SELECT $1::smallint", params, 1, result));
        CHECK(uqa_value_get_int16(cell(result, 0), &integer));
        assert(integer == 42);
    }
    CHECK(uqa_value_destroy(&value));
    assert(uqa_value_create_text_n("a\0b", 3, &value) == UQA_ERROR_INVALID_ARGUMENT);
    assert(uqa_value_create_bool(2, &value) == UQA_ERROR_INVALID_ARGUMENT);
    {
        float vector[] = {1.0f, 2.0f};
        uqa_buffer buffer = NULL;
        uqa_bytes_view bytes = {NULL, 0};
        char formatted[64] = {0};
        double first = 0.0;
        double second = 0.0;
        int consumed = 0;
        CHECK(uqa_value_create_vector_f32(vector, 2, &value));
        kind(value, UQA_TYPE_VECTOR);
        CHECK(uqa_value_create_copy(value, &copy));
        vector[0] = 9.0f;
        CHECK(uqa_value_destroy(&value));
        kind(copy, UQA_TYPE_VECTOR);
        assert(uqa_value_get_text(copy, &text) == UQA_ERROR_TYPE);
        CHECK(uqa_buffer_create(&buffer));
        CHECK(uqa_value_format_sql(copy, buffer));
        CHECK(uqa_buffer_get_bytes(buffer, &bytes));
        assert(bytes.size < sizeof(formatted));
        memcpy(formatted, bytes.data, bytes.size);
        assert(sscanf(formatted, " [ %lf , %lf ] %n", &first, &second, &consumed) == 2);
        assert(first == 1.0 && second == 2.0 && (size_t)consumed == bytes.size);
        CHECK(uqa_buffer_destroy(&buffer));
        CHECK(uqa_value_destroy(&copy));
        vector[0] = NAN;
        assert(uqa_value_create_vector_f32(vector, 2, &value) == UQA_ERROR_SQL);
        assert(value == NULL);
        assert(uqa_value_create_vector_f32(NULL, 0, &value) == UQA_ERROR_SQL);
    }
}

static void stored_values(uqa_engine engine, uqa_result result, int create) {
    const char *exact = "123456789012345678901234567890.0012300";
    const uint8_t payload[] = {0, 1, 255};
    uqa_string_view text = {NULL, 0};
    uqa_bytes_view bytes = {NULL, 0};
    int16_t integer = 0;
    float real = 0.0f;
    uint8_t boolean = 0;
    if (create) {
        uqa_value values[8] = {NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL};
        CHECK(uqa_engine_execute(engine,
            "CREATE TABLE c_scalars(i smallint, r real, n numeric, j json, jb jsonb, b bytea, t text, flag boolean)", result));
        CHECK(uqa_value_create_int16(INT16_MIN, &values[0]));
        CHECK(uqa_value_create_float32(0.1f, &values[1]));
        CHECK(uqa_value_create_numeric(exact, &values[2]));
        CHECK(uqa_value_create_json("[1, 2, 3]", &values[3]));
        CHECK(uqa_value_create_jsonb("{\"n\":1.2300}", &values[4]));
        CHECK(uqa_value_create_bytes(payload, sizeof(payload), &values[5]));
        CHECK(uqa_value_create_text("fixed text", &values[6]));
        CHECK(uqa_value_create_bool(1, &values[7]));
        {
            uqa_value_view params[] = {values[0], values[1], values[2], values[3],
                                       values[4], values[5], values[6], values[7]};
            CHECK(uqa_engine_execute_params(engine,
                "INSERT INTO c_scalars VALUES ($1,$2,$3,$4,$5,$6,$7,$8)", params, 8, result));
        }
        for (size_t index = 0; index < 8; ++index) CHECK(uqa_value_destroy(&values[index]));
    }
    CHECK(uqa_engine_execute(engine, "SELECT * FROM c_scalars", result));
    kind(cell(result, 0), UQA_TYPE_INT16);
    CHECK(uqa_value_get_int16(cell(result, 0), &integer));
    assert(integer == INT16_MIN);
    kind(cell(result, 1), UQA_TYPE_FLOAT32);
    CHECK(uqa_value_get_float32(cell(result, 1), &real));
    assert(real == 0.1f);
    CHECK(uqa_value_get_numeric(cell(result, 2), &text));
    assert(equal(text, exact));
    CHECK(uqa_value_get_json(cell(result, 3), &text));
    assert(equal(text, "[1, 2, 3]"));
    CHECK(uqa_value_get_jsonb(cell(result, 4), &text));
    assert(equal(text, "{\"n\": 1.2300}"));
    CHECK(uqa_value_get_bytes(cell(result, 5), &bytes));
    assert(bytes.size == sizeof(payload) && memcmp(bytes.data, payload, bytes.size) == 0);
    CHECK(uqa_value_get_text(cell(result, 6), &text));
    assert(equal(text, "fixed text"));
    CHECK(uqa_value_get_bool(cell(result, 7), &boolean));
    assert(boolean == 1);
}

int main(void) {
    uqa_engine engine = NULL;
    uqa_result result = NULL;
    const char *directory = getenv("UQA_TEST_DATABASE_DIR");
    char path[4096];
    int length;
    assert(directory != NULL);
    length = snprintf(path, sizeof(path), "%s/values", directory);
    assert(length > 0 && (size_t)length < sizeof(path));
    CHECK(test_database_open_path(path, NULL, &engine));
    CHECK(uqa_result_create(&result));
    integer_widths(engine, result);
    floats(engine, result);
    numeric(engine, result);
    json_values(engine, result);
    nulls_text_and_vectors(engine, result);
    stored_values(engine, result, 1);
    CHECK(uqa_result_destroy(&result));
    CHECK(uqa_engine_close(&engine));
    if (test_database_flags() != UQA_OPEN_MEMORY) {
        CHECK(test_database_open_path(path, NULL, &engine));
        CHECK(uqa_result_create(&result));
        stored_values(engine, result, 0);
        CHECK(uqa_result_destroy(&result));
        CHECK(uqa_engine_close(&engine));
    }
    return 0;
}
