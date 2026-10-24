/*
 * Unified Query Algebra
 *
 * Copyright (c) 2023-2026 Cognica, Inc.
 */

#include <uqa/uqa.h>
#include "test_database.h"
#include <assert.h>
#include <float.h>
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static void checked(uqa_status status, int line) {
    if (status != UQA_OK) {
        uqa_error error = NULL;
        uqa_string_view message = {NULL, 0};
        if (uqa_error_get_last(&error) == UQA_OK) (void)uqa_error_get_message(error, &message);
        fprintf(stderr, "line %d: status %d: %.*s\n", line, (int)status,
                (int)message.size, message.data == NULL ? "" : message.data);
        abort();
    }
}
#define CHECK(call) checked((call), __LINE__)

static const float expected[6] = {-0.0f, FLT_MIN, FLT_MAX, 1.5f, -2.0f, 3.0f};

static void descriptor(uqa_value_view value, uint32_t expected_kind) {
    uqa_type type = NULL;
    uint32_t kind = 99, dimensions = 99;
    CHECK(uqa_value_get_type(value, &type));
    CHECK(uqa_type_get_kind(type, &kind));
    CHECK(uqa_type_get_dimensions(type, &dimensions));
    assert(kind == expected_kind && dimensions == 3);
}

static void vector(uqa_value_view value) {
    uqa_vector_view first = {NULL, 0}, second = {NULL, 0};
    descriptor(value, UQA_TYPE_VECTOR);
    CHECK(uqa_value_get_vector_f32(value, &first));
    CHECK(uqa_value_get_vector_f32(value, &second));
    assert(first.dimensions == 3 && first.data == second.data);
    assert(memcmp(first.data, expected, 3 * sizeof(float)) == 0);
}

static void tensor(uqa_value_view value, int empty) {
    uqa_tensor_view first = {NULL, 0, 0}, second = {NULL, 0, 0};
    descriptor(value, UQA_TYPE_TENSOR);
    CHECK(uqa_value_get_tensor_f32(value, &first));
    CHECK(uqa_value_get_tensor_f32(value, &second));
    assert(first.dimensions == 3 && first.data == second.data);
    assert(first.row_count == (empty ? 0 : 2));
    if (empty) assert(first.data == NULL);
    else assert(memcmp(first.data, expected, sizeof(expected)) == 0);
}

static void stored(uqa_engine engine, uqa_result result) {
    uqa_value_view value = NULL;
    CHECK(uqa_engine_execute(engine, "SELECT v, t, e FROM dense_data", result));
    CHECK(uqa_result_get_value(result, 0, 0, &value));
    vector(value);
    CHECK(uqa_result_get_value(result, 0, 1, &value));
    tensor(value, 0);
    CHECK(uqa_result_get_value(result, 0, 2, &value));
    tensor(value, 1);
}

static void input_errors(void) {
    uqa_value value = NULL;
    const float invalid[] = {NAN, INFINITY, -INFINITY};
    assert(uqa_value_create_tensor_f32(NULL, UINT64_MAX, 3, &value) == UQA_ERROR_RANGE);
    assert(value == NULL);
    assert(uqa_value_create_tensor_f32(NULL, 1, 3, &value) == UQA_ERROR_INVALID_ARGUMENT);
    assert(value == NULL);
    assert(uqa_value_create_tensor_f32(NULL, 0, 0, &value) == UQA_ERROR_SQL);
    assert(value == NULL);
    for (size_t i = 0; i < 3; ++i) {
        assert(uqa_value_create_tensor_f32(invalid + i, 1, 1, &value) == UQA_ERROR_SQL);
        assert(value == NULL);
    }
}

static void binding(uqa_engine engine, uqa_result result) {
    float data[6];
    uqa_value v = NULL, t = NULL, e = NULL, copy = NULL;
    uqa_value_view params[3], value = NULL;
    uqa_statement statement = NULL;
    uqa_cursor cursor = NULL;
    uqa_type type = NULL;
    uint32_t dimensions = 0;
    memcpy(data, expected, sizeof(data));
    CHECK(uqa_value_create_vector_f32(data, 3, &v));
    CHECK(uqa_value_create_tensor_f32(data, 2, 3, &t));
    CHECK(uqa_value_create_tensor_f32(NULL, 0, 3, &e));
    memset(data, 0, sizeof(data));
    vector(v);
    tensor(t, 0);
    tensor(e, 1);
    CHECK(uqa_value_create_copy(t, &copy));
    CHECK(uqa_value_destroy(&t));
    tensor(copy, 0);
    params[0] = v;
    params[1] = copy;
    params[2] = e;
    CHECK(uqa_engine_execute_params(engine, "SELECT $1, $2, $3", params, 3, result));
    CHECK(uqa_result_get_value(result, 0, 0, &value));
    vector(value);
    CHECK(uqa_result_get_value(result, 0, 1, &value));
    tensor(value, 0);
    CHECK(uqa_result_get_value(result, 0, 2, &value));
    tensor(value, 1);
    CHECK(uqa_engine_execute(engine,
        "CREATE TABLE dense_data(v vector(3), t tensor(3), e tensor(3))", result));
    CHECK(uqa_engine_execute_params(engine,
        "INSERT INTO dense_data VALUES ($1, $2, $3)", params, 3, result));
    CHECK(uqa_statement_prepare(engine, "SELECT $1::tensor(3)", &statement));
    CHECK(uqa_statement_get_parameter_type(statement, 1, &type));
    CHECK(uqa_type_get_dimensions(type, &dimensions));
    assert(dimensions == 3);
    CHECK(uqa_statement_bind(statement, 1, e));
    CHECK(uqa_cursor_open_params(engine, "SELECT $1", &params[2], 1, &cursor));
    CHECK(uqa_value_destroy(&v));
    CHECK(uqa_value_destroy(&copy));
    CHECK(uqa_value_destroy(&e));
    CHECK(uqa_statement_execute(statement, result));
    CHECK(uqa_result_get_value(result, 0, 0, &value));
    tensor(value, 1);
    CHECK(uqa_statement_finalize(&statement));
    CHECK(uqa_cursor_get_column_type(cursor, 0, &type));
    CHECK(uqa_type_get_dimensions(type, &dimensions));
    assert(dimensions == 3);
    CHECK(uqa_cursor_fetch(cursor, 1, result));
    CHECK(uqa_result_get_value(result, 0, 0, &value));
    tensor(value, 1);
    CHECK(uqa_value_create_copy(value, &copy));
    CHECK(uqa_cursor_close(&cursor));
    stored(engine, result);
    tensor(copy, 1);
    CHECK(uqa_value_destroy(&copy));
}

static void typed_access(uqa_engine engine, uqa_result result) {
    uqa_value_view value = NULL;
    uqa_vector_view v = {expected, 99};
    uqa_tensor_view t = {expected, 99, 99};
    uqa_type domain = NULL, base = NULL;
    uint32_t dimensions = 99;
    CHECK(uqa_engine_execute(engine, "SELECT NULL::vector(3), NULL::tensor(3), ARRAY[1,2,3]", result));
    CHECK(uqa_result_get_value(result, 0, 0, &value));
    descriptor(value, UQA_TYPE_VECTOR);
    assert(uqa_value_get_vector_f32(value, &v) == UQA_ERROR_NULL);
    assert(v.data == NULL && v.dimensions == 0);
    CHECK(uqa_result_get_value(result, 0, 1, &value));
    descriptor(value, UQA_TYPE_TENSOR);
    assert(uqa_value_get_tensor_f32(value, &t) == UQA_ERROR_NULL);
    assert(t.data == NULL && t.row_count == 0 && t.dimensions == 0);
    CHECK(uqa_result_get_value(result, 0, 2, &value));
    assert(uqa_value_get_vector_f32(value, &v) == UQA_ERROR_TYPE);
    assert(uqa_value_get_tensor_f32(value, &t) == UQA_ERROR_TYPE);
    CHECK(uqa_value_get_type(value, &base));
    assert(uqa_type_get_dimensions(base, &dimensions) == UQA_DONE && dimensions == 0);
    CHECK(uqa_engine_execute(engine, "CREATE DOMAIN tensor_domain AS tensor(3)", result));
    CHECK(uqa_engine_execute(engine, "SELECT t::tensor_domain FROM dense_data", result));
    CHECK(uqa_result_get_value(result, 0, 0, &value));
    CHECK(uqa_value_get_type(value, &domain));
    CHECK(uqa_type_get_base_type(domain, &base));
    CHECK(uqa_type_get_dimensions(base, &dimensions));
    assert(dimensions == 3);
    CHECK(uqa_value_get_tensor_f32(value, &t));
    assert(t.row_count == 2 && t.dimensions == 3);
    assert(memcmp(t.data, expected, sizeof(expected)) == 0);
    assert(uqa_value_get_vector_f32(value, &v) == UQA_ERROR_TYPE);
}

int main(void) {
    char path[4096];
    int length = snprintf(path, sizeof(path), "%s/dense-data", getenv("UQA_TEST_DATABASE_DIR"));
    uqa_engine engine = NULL;
    uqa_result result = NULL;
    assert(length > 0 && (size_t)length < sizeof(path));
    input_errors();
    CHECK(test_database_open_path(path, NULL, &engine));
    CHECK(uqa_result_create(&result));
    binding(engine, result);
    typed_access(engine, result);
    CHECK(uqa_result_destroy(&result));
    CHECK(uqa_engine_close(&engine));
    if (test_database_flags() != UQA_OPEN_MEMORY) {
        CHECK(test_database_open_path(path, NULL, &engine));
        CHECK(uqa_result_create(&result));
        stored(engine, result);
        CHECK(uqa_result_destroy(&result));
        CHECK(uqa_engine_close(&engine));
    }
    return 0;
}
