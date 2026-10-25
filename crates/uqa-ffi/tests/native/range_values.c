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

#ifdef __cplusplus
#define STATIC_ASSERT static_assert
#else
#define STATIC_ASSERT _Static_assert
#endif
STATIC_ASSERT(offsetof(uqa_range_bounds, lower) == 0, "lower bound layout");
STATIC_ASSERT(offsetof(uqa_range_bounds, upper) == sizeof(uqa_value_view), "upper bound layout");
STATIC_ASSERT(offsetof(uqa_range_bounds, flags) == 2 * sizeof(uqa_value_view), "range flags layout");

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

static int equal(uqa_string_view actual, const char *expected) {
    return actual.size == strlen(expected) && (actual.size == 0 || memcmp(actual.data, expected, actual.size) == 0);
}

static void sql_error(uqa_status status, const char *expected) {
    uqa_error error = NULL;
    uqa_string_view state = {NULL, 0};
    assert(status == UQA_ERROR_SQL);
    CHECK(uqa_error_get_last(&error));
    CHECK(uqa_error_get_sqlstate(error, &state));
    assert(equal(state, expected));
}

static void formatted(uqa_value_view value, const char *expected) {
    uqa_buffer buffer = NULL;
    uqa_bytes_view text = {NULL, 0};
    CHECK(uqa_buffer_create(&buffer));
    CHECK(uqa_value_format_sql(value, buffer));
    CHECK(uqa_buffer_get_bytes(buffer, &text));
    if (text.size != strlen(expected) || memcmp(text.data, expected, text.size) != 0) {
        fprintf(stderr, "expected %s, got %.*s\n", expected, (int)text.size, (const char *)text.data);
        abort();
    }
    CHECK(uqa_buffer_destroy(&buffer));
}

static void bounds(uqa_value_view value, const char *lower, const char *upper,
                   uqa_range_flags flags, uint32_t subtype) {
    uqa_range_bounds actual = {NULL, NULL, 0}, again = {NULL, NULL, 0};
    uqa_type type = NULL, endpoint = NULL;
    uint32_t kind = 0, count = 0;
    CHECK(uqa_value_get_range_bounds(value, &actual));
    CHECK(uqa_value_get_range_bounds(value, &again));
    assert(actual.lower == again.lower && actual.upper == again.upper);
    assert(actual.flags == flags);
    if (lower == NULL) assert(actual.lower == NULL); else formatted(actual.lower, lower);
    if (upper == NULL) assert(actual.upper == NULL); else formatted(actual.upper, upper);
    CHECK(uqa_value_get_type(value, &type));
    CHECK(uqa_type_get_range_subtype(type, &endpoint));
    CHECK(uqa_type_get_kind(endpoint, &kind));
    assert(kind == subtype);
    assert(uqa_type_get_range_type(type, &endpoint) == UQA_DONE && endpoint == NULL);
    assert(uqa_value_get_range_count(value, &count) == UQA_ERROR_TYPE && count == 0);
    if (actual.lower != NULL) {
        CHECK(uqa_value_get_type(actual.lower, &endpoint));
        CHECK(uqa_type_get_kind(endpoint, &kind));
        assert(kind == subtype);
    }
}

static uqa_value range(uqa_engine engine, const char *type, const char *lower,
                       const char *upper, uqa_range_flags flags) {
    uqa_value low = NULL, high = NULL, output = NULL;
    if (lower != NULL) CHECK(uqa_value_create_text(lower, &low));
    if (upper != NULL) CHECK(uqa_value_create_text(upper, &high));
    CHECK(uqa_value_create_range_n(engine, type, strlen(type), low, high, flags, &output));
    CHECK(uqa_value_destroy(&low));
    CHECK(uqa_value_destroy(&high));
    return output;
}

static void families(uqa_engine engine, uqa_result result) {
    static const struct {
        const char *type, *multi, *lower, *upper, *value, *out_lower, *out_upper;
        uint32_t kind, input_flags, output_flags;
    } cases[] = {
        {"int4range", "int4multirange", "1", "4", "[2,5)", "2", "5", UQA_TYPE_INT32, UQA_RANGE_UPPER_INCLUSIVE, UQA_RANGE_LOWER_INCLUSIVE},
        {"int8range", "int8multirange", "9223372036854775805", "9223372036854775806", "[9223372036854775805,9223372036854775807)", "9223372036854775805", "9223372036854775807", UQA_TYPE_INT64, 6, 2},
        {"numrange", "nummultirange", "1.2300", "9.870", "[1.2300,9.870]", "1.2300", "9.870", UQA_TYPE_NUMERIC, 6, 6},
        {"daterange", "datemultirange", "2020-01-01", "2020-01-03", "[2020-01-01,2020-01-04)", "2020-01-01", "2020-01-04", UQA_TYPE_DATE, 6, 2},
        {"tsrange", "tsmultirange", "2020-01-01 01:02:03.123456", "2020-01-02 03:04:05", "[\"2020-01-01 01:02:03.123456\",\"2020-01-02 03:04:05\"]", "2020-01-01 01:02:03.123456", "2020-01-02 03:04:05", UQA_TYPE_TIMESTAMP, 6, 6},
        {"tstzrange", "tstzmultirange", "2020-01-01 01:00:00+02", "2020-01-02 03:00:00+03", "[\"2019-12-31 23:00:00+00\",\"2020-01-02 00:00:00+00\")", "2019-12-31 23:00:00+00", "2020-01-02 00:00:00+00", UQA_TYPE_TIMESTAMPTZ, 2, 2}
    };
    CHECK(uqa_engine_execute(engine, "SET TIME ZONE 'UTC'", result));
    for (size_t i = 0; i < sizeof(cases) / sizeof(cases[0]); ++i) {
        uqa_value owned = range(engine, cases[i].type, cases[i].lower, cases[i].upper, cases[i].input_flags);
        uqa_value multi = NULL, copy = NULL;
        uqa_value_view view = owned, member = NULL, again = NULL;
        uqa_type type = NULL, member_type = NULL, endpoint = NULL;
        uint32_t kind = 0, count = 0;
        formatted(owned, cases[i].value);
        bounds(owned, cases[i].out_lower, cases[i].out_upper, cases[i].output_flags, cases[i].kind);
        CHECK(uqa_value_create_multirange_n(engine, cases[i].multi, strlen(cases[i].multi), &view, 1, &multi));
        CHECK(uqa_value_destroy(&owned));
        CHECK(uqa_value_get_range_count(multi, &count));
        assert(count == 1);
        CHECK(uqa_value_get_type(multi, &type));
        CHECK(uqa_type_get_range_type(type, &member_type));
        CHECK(uqa_type_get_kind(member_type, &kind));
        assert(kind == UQA_TYPE_RANGE);
        CHECK(uqa_type_get_range_subtype(type, &endpoint));
        CHECK(uqa_type_get_kind(endpoint, &kind));
        assert(kind == cases[i].kind);
        CHECK(uqa_value_get_range(multi, 0, &member));
        CHECK(uqa_value_get_range(multi, 0, &again));
        assert(member == again);
        bounds(member, cases[i].out_lower, cases[i].out_upper, cases[i].output_flags, cases[i].kind);
        CHECK(uqa_value_create_copy(member, &copy));
        CHECK(uqa_value_destroy(&multi));
        view = copy;
        CHECK(uqa_engine_execute_params(engine, "SELECT $1", &view, 1, result));
        CHECK(uqa_value_destroy(&copy));
        CHECK(uqa_result_get_value(result, 0, 0, &view));
        formatted(view, cases[i].value);
        bounds(view, cases[i].out_lower, cases[i].out_upper, cases[i].output_flags, cases[i].kind);
    }
}

static void special_values(uqa_engine engine) {
    uqa_value value = range(engine, "int4range", NULL, NULL, 6);
    uqa_value point = NULL, null_value = NULL;
    uqa_value_view member = NULL;
    uqa_range_bounds actual = {NULL, NULL, 0};
    uint32_t count = 99;
    bounds(value, NULL, NULL, UQA_RANGE_LOWER_UNBOUNDED | UQA_RANGE_UPPER_UNBOUNDED, UQA_TYPE_INT32);
    formatted(value, "(,)");
    CHECK(uqa_value_get_range_bounds(value, &actual));
    CHECK(uqa_value_create_range(engine, "int4range", actual.lower, actual.upper, actual.flags, &point));
    formatted(point, "(,)");
    CHECK(uqa_value_destroy(&point)); CHECK(uqa_value_destroy(&value));
    value = range(engine, "int4range", NULL, NULL, UQA_RANGE_EMPTY);
    bounds(value, NULL, NULL, UQA_RANGE_EMPTY, UQA_TYPE_INT32);
    formatted(value, "empty"); CHECK(uqa_value_destroy(&value));
    value = range(engine, "int4range", "1", "1", 0);
    bounds(value, NULL, NULL, UQA_RANGE_EMPTY, UQA_TYPE_INT32);
    CHECK(uqa_value_destroy(&value));
    value = range(engine, "numrange", "1.2300", "1.2300", 6);
    bounds(value, "1.2300", "1.2300", 6, UQA_TYPE_NUMERIC);
    CHECK(uqa_value_destroy(&value));
    value = range(engine, "daterange", "-infinity", "infinity", 6);
    bounds(value, "-infinity", "infinity", 6, UQA_TYPE_DATE);
    CHECK(uqa_value_destroy(&value));
    CHECK(uqa_value_create_multirange(engine, "int4multirange", NULL, 0, &value));
    CHECK(uqa_value_get_range_count(value, &count)); assert(count == 0);
    assert(uqa_value_get_range(value, 0, &member) == UQA_ERROR_RANGE && member == NULL);
    assert(uqa_value_get_range_bounds(value, &actual) == UQA_ERROR_TYPE && actual.lower == NULL && actual.flags == 0);
    formatted(value, "{}"); CHECK(uqa_value_destroy(&value));
    CHECK(uqa_value_create_typed_null(engine, "int4range", &null_value));
    assert(uqa_value_get_range_bounds(null_value, &actual) == UQA_ERROR_NULL && actual.flags == 0);
    CHECK(uqa_value_destroy(&null_value));
    CHECK(uqa_value_create_null(&null_value));
    CHECK(uqa_value_create_range(engine, "int4range", null_value, NULL, 6, &value));
    formatted(value, "(,)"); CHECK(uqa_value_destroy(&value));
    CHECK(uqa_value_destroy(&null_value));
}

static void errors(uqa_engine engine) {
    uqa_value one = NULL, four = NULL, maximum = NULL, output = NULL;
    uqa_value null_value = NULL;
    uqa_value_view member;
    uqa_type type = NULL, child = NULL;
    CHECK(uqa_value_create_int32(1, &one)); CHECK(uqa_value_create_int32(4, &four));
    CHECK(uqa_value_create_int32(INT32_MAX, &maximum)); CHECK(uqa_value_create_null(&null_value));
    sql_error(uqa_value_create_range(engine, "int4range", four, one, 0, &output), "22000"); assert(output == NULL);
    sql_error(uqa_value_create_range(engine, "int4range", maximum, maximum, 6, &output), "22003"); assert(output == NULL);
    assert(uqa_value_create_range(engine, "integer", one, four, 2, &output) == UQA_ERROR_SQL && output == NULL);
    assert(uqa_value_create_range(engine, "int4range", one, four, 32, &output) == UQA_ERROR_INVALID_ARGUMENT);
    assert(uqa_value_create_range(engine, "int4range", one, NULL, UQA_RANGE_EMPTY, &output) == UQA_ERROR_INVALID_ARGUMENT);
    assert(uqa_value_create_range(engine, "int4range", one, four, UQA_RANGE_LOWER_UNBOUNDED, &output) == UQA_ERROR_INVALID_ARGUMENT);
    assert(uqa_value_create_range_n(engine, NULL, 0, one, four, 2, &output) == UQA_ERROR_INVALID_ARGUMENT);
    member = null_value;
    sql_error(uqa_value_create_multirange(engine, "int4multirange", &member, 1, &output), "22004"); assert(output == NULL);
    member = one;
    sql_error(uqa_value_create_multirange(engine, "int4multirange", &member, 1, &output), "42846"); assert(output == NULL);
    assert(uqa_value_create_multirange(engine, "int4multirange", NULL, 1, &output) == UQA_ERROR_INVALID_ARGUMENT && output == NULL);
    CHECK(uqa_value_get_type(one, &type));
    assert(uqa_type_get_range_subtype(type, &child) == UQA_DONE && child == NULL);
    CHECK(uqa_value_destroy(&one)); CHECK(uqa_value_destroy(&four));
    CHECK(uqa_value_destroy(&maximum)); CHECK(uqa_value_destroy(&null_value));
}

static void stored(uqa_value_view multi) {
    uint32_t count = 0;
    uqa_value_view member = NULL;
    CHECK(uqa_value_get_range_count(multi, &count)); assert(count == 2);
    CHECK(uqa_value_get_range(multi, 0, &member)); bounds(member, "1", "6", 2, UQA_TYPE_INT32);
    CHECK(uqa_value_get_range(multi, 1, &member)); bounds(member, "9", "12", 2, UQA_TYPE_INT32);
    formatted(multi, "{[1,6),[9,12)}");
}

static void normalization(uqa_engine engine, uqa_result result) {
    uqa_value members[5] = {NULL, NULL, NULL, NULL, NULL}, multi = NULL;
    uqa_value_view inputs[5], view = NULL;
    uqa_statement statement = NULL;
    uqa_cursor cursor = NULL;
    members[0] = range(engine, "int4range", "10", "11", 6);
    members[1] = range(engine, "int4range", "3", "6", 2);
    members[2] = range(engine, "int4range", "1", "3", 2);
    members[3] = range(engine, "int4range", "9", "10", 2);
    members[4] = range(engine, "int4range", NULL, NULL, UQA_RANGE_EMPTY);
    for (size_t i = 0; i < 5; ++i) inputs[i] = members[i];
    CHECK(uqa_value_create_multirange(engine, "int4multirange", inputs, 5, &multi));
    for (size_t i = 0; i < 5; ++i) CHECK(uqa_value_destroy(&members[i]));
    stored(multi);
    CHECK(uqa_engine_execute(engine, "CREATE TABLE c_range_data(v int4multirange)", result));
    CHECK(uqa_statement_prepare(engine, "INSERT INTO c_range_data VALUES ($1)", &statement));
    CHECK(uqa_statement_bind(statement, 1, multi));
    CHECK(uqa_value_destroy(&multi));
    CHECK(uqa_statement_execute(statement, result)); CHECK(uqa_statement_finalize(&statement));
    CHECK(uqa_cursor_open(engine, "SELECT v FROM c_range_data", &cursor));
    CHECK(uqa_cursor_fetch(cursor, 1, result));
    CHECK(uqa_result_get_value(result, 0, 0, &view)); stored(view);
    CHECK(uqa_cursor_close(&cursor));
    CHECK(uqa_engine_execute(engine, "SELECT ARRAY['[1.2,3.4)'::numrange]", result));
    CHECK(uqa_result_get_value(result, 0, 0, &view));
    CHECK(uqa_value_get_element(view, 0, &view)); bounds(view, "1.2", "3.4", 2, UQA_TYPE_NUMERIC);
}

static void domain_effects(uqa_engine engine, uqa_result result) {
    uqa_value range_value = NULL, multi = NULL;
    uqa_value_view member = NULL, value = NULL;
    uqa_range_bounds observed = {NULL, NULL, 0};
    uint32_t state = 0;
    CHECK(uqa_engine_execute(engine,
        "CREATE SEQUENCE c_range_calls; "
        "CREATE DOMAIN c_counted_range AS int4range CHECK (nextval('c_range_calls') > 0 AND NOT isempty(VALUE)); "
        "CREATE DOMAIN c_counted_multi AS int4multirange CHECK (nextval('c_range_calls') > 0 AND NOT isempty(VALUE))", result));
    range_value = range(engine, "c_counted_range", "1", "5", 2);
    CHECK(uqa_value_get_range_bounds(range_value, &observed));
    formatted(observed.lower, "1");
    member = range_value;
    CHECK(uqa_value_create_multirange(engine, "c_counted_multi", &member, 1, &multi));
    CHECK(uqa_value_get_range(multi, 0, &member)); formatted(member, "[1,5)");
    value = multi;
    CHECK(uqa_engine_execute_params(engine, "SELECT $1, currval('c_range_calls')", &value, 1, result));
    CHECK(uqa_result_get_value(result, 0, 1, &value)); formatted(value, "2");
    CHECK(uqa_value_destroy(&multi)); CHECK(uqa_value_destroy(&range_value));
    CHECK(uqa_engine_execute(engine, "BEGIN; SAVEPOINT range_input", result));
    sql_error(uqa_value_create_range(engine, "c_counted_range", NULL, NULL, UQA_RANGE_EMPTY, &range_value), "23514");
    assert(range_value == NULL);
    CHECK(uqa_transaction_get_state(engine, &state)); assert(state == UQA_TRANSACTION_FAILED);
    CHECK(uqa_engine_execute(engine, "ROLLBACK TO range_input; SELECT currval('c_range_calls')", result));
    CHECK(uqa_result_get_value(result, 0, 0, &value)); formatted(value, "3");
    CHECK(uqa_engine_execute(engine, "COMMIT", result));
}

int main(void) {
    char path[4096];
    int length = snprintf(path, sizeof(path), "%s/range-data", getenv("UQA_TEST_DATABASE_DIR"));
    uqa_engine engine = NULL;
    uqa_result result = NULL;
    uqa_value_view value = NULL;
    assert(length > 0 && (size_t)length < sizeof(path));
    CHECK(test_database_open_path(path, NULL, &engine)); CHECK(uqa_result_create(&result));
    families(engine, result); special_values(engine); errors(engine);
    normalization(engine, result); domain_effects(engine, result);
    CHECK(uqa_result_destroy(&result)); CHECK(uqa_engine_close(&engine));
    if (test_database_flags() != UQA_OPEN_MEMORY) {
        CHECK(test_database_open_path(path, NULL, &engine)); CHECK(uqa_result_create(&result));
        CHECK(uqa_engine_execute(engine, "SELECT v FROM c_range_data", result));
        CHECK(uqa_result_get_value(result, 0, 0, &value)); stored(value);
        CHECK(uqa_result_destroy(&result)); CHECK(uqa_engine_close(&engine));
    }
    return 0;
}
