/*
 * Unified Query Algebra
 *
 * Copyright (c) 2023-2026 Cognica, Inc.
 */

#include <uqa/uqa.h>
#include "test_database.h"
#include <assert.h>
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

static void kind(uqa_value_view value, uint32_t expected) {
    uqa_type type = NULL;
    uint32_t actual = 0;
    CHECK(uqa_value_get_type(value, &type));
    CHECK(uqa_type_get_kind(type, &actual));
    assert(actual == expected);
}

static void formatted(uqa_value_view value, const char *expected) {
    uqa_buffer buffer = NULL;
    uqa_bytes_view bytes = {NULL, 0};
    CHECK(uqa_buffer_create(&buffer));
    CHECK(uqa_value_format_sql(value, buffer));
    CHECK(uqa_buffer_get_bytes(buffer, &bytes));
    assert(bytes.size == strlen(expected) && memcmp(bytes.data, expected, bytes.size) == 0);
    CHECK(uqa_buffer_destroy(&buffer));
}

static void matrix(uqa_value_view value) {
    uqa_array_shape shape = {NULL, 0, 0}, repeated = {NULL, 0, 0};
    uqa_type type = NULL, element_type = NULL;
    const int32_t expected[] = {11, 0, 13, 21, 22, 23};
    uint32_t oid = 0;
    CHECK(uqa_value_get_array_shape(value, &shape));
    CHECK(uqa_value_get_array_shape(value, &repeated));
    assert(shape.rank == 2 && shape.element_count == 6);
    assert(shape.dimensions == repeated.dimensions);
    assert(shape.dimensions[0].length == 2 && shape.dimensions[0].lower_bound == -3);
    assert(shape.dimensions[1].length == 3 && shape.dimensions[1].lower_bound == 7);
    CHECK(uqa_value_get_type(value, &type));
    CHECK(uqa_type_get_element_type(type, &element_type));
    CHECK(uqa_type_get_oid(element_type, &oid));
    assert(oid == 23);
    for (uint64_t index = 0; index < 6; ++index) {
        uqa_value_view element = NULL, again = NULL;
        int32_t number = 99;
        uint8_t is_null = 99;
        CHECK(uqa_value_get_element(value, index, &element));
        CHECK(uqa_value_get_element(value, index, &again));
        assert(element == again);
        kind(element, UQA_TYPE_INT32);
        CHECK(uqa_value_get_is_null(element, &is_null));
        assert(is_null == (index == 1));
        if (index == 1) {
            assert(uqa_value_get_int32(element, &number) == UQA_ERROR_NULL && number == 0);
        } else {
            CHECK(uqa_value_get_int32(element, &number));
            assert(number == expected[index]);
        }
    }
    {
        uqa_value_view missing = value;
        assert(uqa_value_get_element(value, 6, &missing) == UQA_ERROR_RANGE && missing == NULL);
        missing = value;
        assert(uqa_value_get_element(value, UINT64_MAX, &missing) == UQA_ERROR_RANGE && missing == NULL);
    }
}

static void binding(uqa_engine engine, uqa_result result) {
    uqa_value array = NULL, copy = NULL, retained_element = NULL;
    uqa_value_view parameter = NULL, value = NULL, element = NULL;
    uqa_statement statement = NULL;
    uqa_cursor cursor = NULL;
    {
        const int32_t numbers[] = {11, 0, 13, 21, 22, 23};
        uqa_array_dimension dimensions[] = {{2, -3}, {3, 7}};
        uqa_value owners[6] = {NULL, NULL, NULL, NULL, NULL, NULL};
        uqa_value_view elements[6];
        for (int i = 0; i < 6; ++i) {
            if (i == 1) CHECK(uqa_value_create_null(&owners[i]));
            else CHECK(uqa_value_create_int32(numbers[i], &owners[i]));
            elements[i] = owners[i];
        }
        CHECK(uqa_value_create_array_n(engine, "integer[][][]suffix", 13, dimensions, 2, elements, 6, &array));
        dimensions[0].lower_bound = 99;
        for (int i = 0; i < 6; ++i) CHECK(uqa_value_destroy(&owners[i]));
    }
    matrix(array);
    CHECK(uqa_value_get_element(array, 5, &element));
    CHECK(uqa_value_create_copy(element, &retained_element));
    CHECK(uqa_value_create_copy(array, &copy));
    CHECK(uqa_value_destroy(&array));
    matrix(copy);
    CHECK(uqa_engine_execute(engine, "CREATE TABLE array_data(v integer[])", result));
    parameter = copy;
    CHECK(uqa_engine_execute_params(engine, "INSERT INTO array_data VALUES ($1)", &parameter, 1, result));
    CHECK(uqa_statement_prepare(engine, "SELECT $1::integer[]", &statement));
    CHECK(uqa_statement_bind(statement, 1, copy));
    CHECK(uqa_cursor_open_params(engine, "SELECT $1", &parameter, 1, &cursor));
    CHECK(uqa_value_destroy(&copy));
    CHECK(uqa_statement_execute(statement, result));
    CHECK(uqa_result_get_value(result, 0, 0, &value));
    matrix(value);
    CHECK(uqa_statement_finalize(&statement));
    CHECK(uqa_cursor_fetch(cursor, 1, result));
    CHECK(uqa_result_get_value(result, 0, 0, &value));
    matrix(value);
    CHECK(uqa_cursor_close(&cursor));
    CHECK(uqa_engine_execute(engine, "SELECT 0", result));
    formatted(retained_element, "23");
    CHECK(uqa_value_destroy(&retained_element));
}

static void empty_and_catalog_vectors(uqa_engine engine, uqa_result result) {
    uqa_value_view value = NULL, element = NULL;
    uqa_array_shape shape = {NULL, 0, 0};
    CHECK(uqa_engine_execute(engine,
        "SELECT '{}'::integer[], NULL::integer[], ''::int2vector, '0 4294967295'::oidvector, "
        "ARRAY['1 2'::int2vector, ''::int2vector], 1, ARRAY[ARRAY[1,2],ARRAY[3,4]]", result));
    CHECK(uqa_result_get_value(result, 0, 0, &value));
    CHECK(uqa_value_get_array_shape(value, &shape));
    assert(shape.rank == 0 && shape.element_count == 0 && shape.dimensions == NULL);
    assert(uqa_value_get_element(value, 0, &element) == UQA_ERROR_RANGE && element == NULL);
    CHECK(uqa_result_get_value(result, 0, 1, &value));
    assert(uqa_value_get_array_shape(value, &shape) == UQA_ERROR_NULL);
    assert(shape.rank == 0 && shape.element_count == 0 && shape.dimensions == NULL);
    assert(uqa_value_get_element(value, 0, &element) == UQA_ERROR_NULL && element == NULL);
    CHECK(uqa_result_get_value(result, 0, 2, &value));
    CHECK(uqa_value_get_array_shape(value, &shape));
    assert(shape.rank == 1 && shape.element_count == 0);
    assert(shape.dimensions[0].lower_bound == 0 && shape.dimensions[0].length == 0);
    CHECK(uqa_result_get_value(result, 0, 3, &value));
    CHECK(uqa_value_get_array_shape(value, &shape));
    assert(shape.rank == 1 && shape.element_count == 2 && shape.dimensions[0].lower_bound == 0);
    CHECK(uqa_value_get_element(value, 1, &element));
    kind(element, UQA_TYPE_OID);
    formatted(element, "4294967295");
    CHECK(uqa_result_get_value(result, 0, 4, &value));
    CHECK(uqa_value_get_array_shape(value, &shape));
    assert(shape.rank == 1 && shape.element_count == 2);
    CHECK(uqa_value_get_element(value, 0, &element));
    kind(element, UQA_TYPE_INT2VECTOR);
    CHECK(uqa_value_get_array_shape(element, &shape));
    assert(shape.rank == 1 && shape.element_count == 2 && shape.dimensions[0].lower_bound == 0);
    CHECK(uqa_value_get_element(element, 1, &value));
    kind(value, UQA_TYPE_INT16);
    formatted(value, "2");
    CHECK(uqa_result_get_value(result, 0, 5, &value));
    assert(uqa_value_get_array_shape(value, &shape) == UQA_ERROR_TYPE && shape.dimensions == NULL);
    assert(uqa_value_get_element(value, 0, &element) == UQA_ERROR_TYPE && element == NULL);
    CHECK(uqa_result_get_value(result, 0, 6, &value));
    {
        uqa_type type = NULL, leaf = NULL;
        uint32_t oid = 0;
        CHECK(uqa_value_get_type(value, &type));
        CHECK(uqa_type_get_element_type(type, &leaf));
        CHECK(uqa_type_get_oid(leaf, &oid));
        assert(oid == 23);
        CHECK(uqa_value_get_element(value, 3, &element));
        kind(element, UQA_TYPE_INT32);
        formatted(element, "4");
    }
}

static void text_and_binary_children(uqa_engine engine, uqa_result result) {
    const char *expected[] = {"", "NULL", NULL, "é"};
    uqa_value_view array = NULL, element = NULL;
    uqa_array_shape shape = {NULL, 0, 0};
    CHECK(uqa_engine_execute(engine,
        "SELECT ARRAY['','NULL',NULL,'é']::text[], ARRAY['\\x00ff'::bytea], "
        "array_fill(9, ARRAY[1,1,1,1,1,1])", result));
    CHECK(uqa_result_get_value(result, 0, 0, &array));
    for (uint64_t index = 0; index < 4; ++index) {
        uqa_string_view text = {NULL, 0};
        CHECK(uqa_value_get_element(array, index, &element));
        kind(element, UQA_TYPE_TEXT);
        if (expected[index] == NULL) {
            assert(uqa_value_get_text(element, &text) == UQA_ERROR_NULL && text.data == NULL && text.size == 0);
        } else {
            CHECK(uqa_value_get_text(element, &text));
            assert(text.size == strlen(expected[index]));
            if (text.size != 0) assert(memcmp(text.data, expected[index], text.size) == 0);
        }
    }
    CHECK(uqa_result_get_value(result, 0, 1, &array));
    CHECK(uqa_value_get_element(array, 0, &element));
    {
        uqa_bytes_view bytes = {NULL, 0};
        CHECK(uqa_value_get_bytes(element, &bytes));
        assert(bytes.size == 2 && bytes.data[0] == 0 && bytes.data[1] == 255);
    }
    CHECK(uqa_result_get_value(result, 0, 2, &array));
    CHECK(uqa_value_get_array_shape(array, &shape));
    assert(shape.rank == 6 && shape.element_count == 1);
    for (uint32_t i = 0; i < 6; ++i) assert(shape.dimensions[i].length == 1 && shape.dimensions[i].lower_bound == 1);
    CHECK(uqa_value_get_element(array, 0, &element));
    formatted(element, "9");
}

static void catalog_children(uqa_engine engine, uqa_result result) {
    uqa_value value = NULL, retained = NULL;
    uqa_value_view element = NULL, row = NULL;
    CHECK(uqa_engine_execute(engine,
        "CREATE DOMAIN array_positive AS integer CHECK (VALUE > 0); "
        "CREATE DOMAIN positive_array AS array_positive[]; "
        "CREATE TYPE array_mood AS ENUM ('before'); "
        "CREATE TYPE array_record AS (n integer, t text)", result));
    CHECK(uqa_value_create_from_text(engine, "positive_array", "{1,2}", &value));
    CHECK(uqa_value_get_element(value, 1, &element));
    kind(element, UQA_TYPE_DOMAIN);
    formatted(element, "2");
    CHECK(uqa_value_destroy(&value));
    CHECK(uqa_engine_execute(engine,
        "SELECT ARRAY['before'::array_mood], ARRAY[ROW(7,'x')::array_record]", result));
    CHECK(uqa_result_get_value(result, 0, 0, &row));
    CHECK(uqa_value_get_element(row, 0, &element));
    kind(element, UQA_TYPE_ENUM);
    CHECK(uqa_value_create_copy(element, &retained));
    CHECK(uqa_result_get_value(result, 0, 1, &row));
    CHECK(uqa_value_get_element(row, 0, &element));
    kind(element, UQA_TYPE_COMPOSITE);
    formatted(element, "(7,x)");
    CHECK(uqa_engine_execute(engine, "ALTER TYPE array_mood RENAME VALUE 'before' TO 'after'", result));
    formatted(retained, "after");
    CHECK(uqa_value_destroy(&retained));
}

static void array_input(uqa_engine engine, uqa_result result) {
    uqa_value one = NULL, zero = NULL, array = NULL, first = NULL, second = NULL;
    uqa_value_view elements[3], value = NULL, child = NULL;
    uqa_array_dimension dimensions[] = {{2, -1}};
    uqa_array_shape shape = {NULL, 0, 0};
    CHECK(uqa_value_create_int32(1, &one));
    CHECK(uqa_value_create_int32(0, &zero));
    elements[0] = one; elements[1] = one; elements[2] = one;
    assert(uqa_value_create_array(engine, "integer[]", dimensions, 1, elements, 1, &array) == UQA_ERROR_SQL && array == NULL);
    assert(uqa_value_create_array(engine, "integer", dimensions, 1, elements, 2, &array) == UQA_ERROR_SQL && array == NULL);
    assert(uqa_value_create_array(engine, "integer[]", NULL, 1, elements, 2, &array) == UQA_ERROR_INVALID_ARGUMENT && array == NULL);
    assert(uqa_value_create_array(engine, "integer[]", dimensions, 1, NULL, 2, &array) == UQA_ERROR_INVALID_ARGUMENT && array == NULL);
    CHECK(uqa_value_create_array(engine, "integer[]", NULL, 0, NULL, 0, &array));
    CHECK(uqa_value_get_array_shape(array, &shape));
    assert(shape.rank == 0 && shape.element_count == 0);
    CHECK(uqa_value_destroy(&array));
    CHECK(uqa_engine_execute(engine,
        "CREATE SEQUENCE array_input_calls; "
        "CREATE DOMAIN counted_element AS integer CHECK (nextval('array_input_calls') > 0 AND VALUE > 0); "
        "CREATE DOMAIN counted_array AS counted_element[] CHECK (cardinality(VALUE) = 2)", result));
    CHECK(uqa_value_create_array(engine, "counted_array", dimensions, 1, elements, 2, &array));
    formatted(array, "[-1:0]={1,1}");
    value = array;
    CHECK(uqa_engine_execute_params(engine, "SELECT $1, currval('array_input_calls')", &value, 1, result));
    CHECK(uqa_result_get_value(result, 0, 1, &value));
    formatted(value, "2");
    CHECK(uqa_value_destroy(&array));
    elements[1] = zero;
    dimensions[0].length = 3;
    CHECK(uqa_engine_execute(engine, "BEGIN; SAVEPOINT before_array", result));
    assert(uqa_value_create_array(engine, "counted_element[]", dimensions, 1, elements, 3, &array) == UQA_ERROR_SQL && array == NULL);
    assert(uqa_engine_execute(engine, "SELECT 1", result) == UQA_ERROR_SQL);
    CHECK(uqa_engine_execute(engine, "ROLLBACK TO before_array; SELECT currval('array_input_calls')", result));
    CHECK(uqa_result_get_value(result, 0, 0, &value));
    formatted(value, "4");
    CHECK(uqa_engine_execute(engine, "COMMIT; CREATE DOMAIN array_leaf AS integer[]; CREATE TABLE array_domain_data(v array_leaf[])", result));
    CHECK(uqa_value_create_from_text(engine, "array_leaf", "{1,2}", &first));
    CHECK(uqa_value_create_from_text(engine, "array_leaf", "{3}", &second));
    elements[0] = first; elements[1] = second;
    dimensions[0].length = 2; dimensions[0].lower_bound = 1;
    CHECK(uqa_value_create_array(engine, "array_leaf[]", dimensions, 1, elements, 2, &array));
    CHECK(uqa_value_destroy(&first)); CHECK(uqa_value_destroy(&second));
    formatted(array, "{\"{1,2}\",\"{3}\"}");
    CHECK(uqa_value_get_array_shape(array, &shape));
    assert(shape.rank == 1 && shape.element_count == 2);
    CHECK(uqa_value_get_element(array, 0, &child));
    kind(child, UQA_TYPE_DOMAIN);
    CHECK(uqa_value_get_array_shape(child, &shape));
    assert(shape.rank == 1 && shape.element_count == 2);
    CHECK(uqa_value_get_element(child, 1, &value));
    formatted(value, "2");
    value = array;
    CHECK(uqa_engine_execute_params(engine, "INSERT INTO array_domain_data VALUES ($1)", &value, 1, result));
    CHECK(uqa_value_destroy(&array));
    CHECK(uqa_engine_execute(engine,
        "SELECT ARRAY[ARRAY[1,2]::array_leaf,ARRAY[3]::array_leaf], '{\"{1,2}\",\"{3}\"}'::array_leaf[]", result));
    for (uint32_t i = 0; i < 2; ++i) {
        CHECK(uqa_result_get_value(result, 0, i, &value));
        formatted(value, "{\"{1,2}\",\"{3}\"}");
        CHECK(uqa_value_get_array_shape(value, &shape));
        assert(shape.rank == 1 && shape.element_count == 2);
    }
    CHECK(uqa_value_destroy(&one)); CHECK(uqa_value_destroy(&zero));
}

int main(void) {
    char path[4096];
    int length = snprintf(path, sizeof(path), "%s/array-data", getenv("UQA_TEST_DATABASE_DIR"));
    uqa_engine engine = NULL;
    uqa_result result = NULL;
    uqa_value_view value = NULL;
    assert(length > 0 && (size_t)length < sizeof(path));
    CHECK(test_database_open_path(path, NULL, &engine));
    CHECK(uqa_result_create(&result));
    binding(engine, result);
    empty_and_catalog_vectors(engine, result);
    text_and_binary_children(engine, result);
    catalog_children(engine, result);
    array_input(engine, result);
    CHECK(uqa_engine_execute(engine, "SELECT v FROM array_data", result));
    CHECK(uqa_result_get_value(result, 0, 0, &value));
    matrix(value);
    CHECK(uqa_result_destroy(&result));
    CHECK(uqa_engine_close(&engine));
    if (test_database_flags() != UQA_OPEN_MEMORY) {
        CHECK(test_database_open_path(path, NULL, &engine));
        CHECK(uqa_result_create(&result));
        CHECK(uqa_engine_execute(engine, "SELECT v FROM array_data", result));
        CHECK(uqa_result_get_value(result, 0, 0, &value));
        matrix(value);
        CHECK(uqa_engine_execute(engine, "SELECT v FROM array_domain_data", result));
        CHECK(uqa_result_get_value(result, 0, 0, &value));
        formatted(value, "{\"{1,2}\",\"{3}\"}");
        CHECK(uqa_result_destroy(&result));
        CHECK(uqa_engine_close(&engine));
    }
    return 0;
}
