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

/* Independent PostgreSQL 18.4 observations are retained in the Engine owner's
 * src/tests/parameters.pg18.json, including domain input transaction effects. */

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

static int equal(uqa_string_view actual, const char *expected) {
    size_t size = strlen(expected);
    return actual.size == size && (size == 0 || memcmp(actual.data, expected, size) == 0);
}

static void sql_error(uqa_status status, const char *state) {
    uqa_error error = NULL;
    uqa_string_view actual = {NULL, 0};
    assert(status == UQA_ERROR_SQL);
    CHECK(uqa_error_get_last(&error));
    CHECK(uqa_error_get_sqlstate(error, &actual));
    assert(equal(actual, state));
}

static void formatted(uqa_value_view value, uqa_buffer buffer, const char *expected) {
    uqa_bytes_view bytes = {NULL, 0};
    CHECK(uqa_value_format_sql(value, buffer));
    CHECK(uqa_buffer_get_bytes(buffer, &bytes));
    assert(bytes.size == strlen(expected));
    assert(bytes.size == 0 || memcmp(bytes.data, expected, bytes.size) == 0);
}

static int64_t count(uqa_engine engine, uqa_result result) {
    uqa_value_view value = NULL;
    int64_t number = 0;
    CHECK(uqa_engine_execute(engine, "SELECT count(*) FROM c_host_input_ref.audit", result));
    CHECK(uqa_result_get_value(result, 0, 0, &value));
    CHECK(uqa_value_get_int64(value, &number));
    return number;
}

static void builtins(uqa_engine engine, uqa_result result, uqa_buffer buffer) {
    uqa_value value = NULL;
    uqa_type type = NULL;
    uqa_type_kind kind = UQA_TYPE_UNKNOWN;
    uint8_t is_null = 0;
    int16_t number = 1;
    {
        uint32_t state = UQA_TRANSACTION_IDLE;
        CHECK(uqa_transaction_begin(engine));
        assert(uqa_value_create_typed_null_n(engine, NULL, 0, &value) == UQA_ERROR_INVALID_ARGUMENT && value == NULL);
        assert(uqa_value_create_from_text_n(engine, NULL, 0, "", 0, &value) == UQA_ERROR_INVALID_ARGUMENT && value == NULL);
        assert(uqa_value_create_from_text_n(engine, "text", 4, NULL, 0, &value) == UQA_ERROR_INVALID_ARGUMENT && value == NULL);
        assert(uqa_value_create_from_text(engine, "text", NULL, &value) == UQA_ERROR_INVALID_ARGUMENT && value == NULL);
        CHECK(uqa_transaction_get_state(engine, &state));
        assert(state == UQA_TRANSACTION_ACTIVE);
        CHECK(uqa_value_create_from_text_n(engine, "text", 4, "", 0, &value));
        CHECK(uqa_value_get_is_null(value, &is_null));
        assert(is_null == 0);
        formatted(value, buffer, "");
        CHECK(uqa_value_destroy(&value));
        CHECK(uqa_transaction_rollback(engine));
    }
    CHECK(uqa_value_create_typed_null(engine, "smallint", &value));
    CHECK(uqa_value_get_is_null(value, &is_null));
    assert(is_null == 1);
    CHECK(uqa_value_get_type(value, &type));
    CHECK(uqa_type_get_kind(type, &kind));
    assert(kind == UQA_TYPE_INT16);
    assert(uqa_value_get_int16(value, &number) == UQA_ERROR_NULL && number == 0);
    {
        uqa_value_view params[] = {value};
        uqa_value_view name = NULL;
        uqa_string_view text = {NULL, 0};
        CHECK(uqa_engine_execute_params(engine, "SELECT pg_typeof($1)::text", params, 1, result));
        CHECK(uqa_result_get_value(result, 0, 0, &name));
        CHECK(uqa_value_get_text(name, &text));
        assert(equal(text, "smallint"));
    }
    CHECK(uqa_value_destroy(&value));
    {
        const char type_name[] = {'n','u','m','e','r','i','c','(','5',',','2',')'};
        const char input[] = {'1','2','.','3','4','5','6'};
        uint32_t precision = 0;
        int32_t scale = 0;
        CHECK(uqa_value_create_from_text_n(engine, type_name, sizeof(type_name), input, sizeof(input), &value));
        formatted(value, buffer, "12.35");
        CHECK(uqa_value_get_type(value, &type));
        CHECK(uqa_type_get_numeric_precision(type, &precision));
        CHECK(uqa_type_get_numeric_scale(type, &scale));
        assert(precision == 5 && scale == 2);
        CHECK(uqa_value_destroy(&value));
        CHECK(uqa_value_create_typed_null_n(engine, type_name, sizeof(type_name), &value));
        CHECK(uqa_value_get_type(value, &type));
        CHECK(uqa_type_get_numeric_scale(type, &scale));
        assert(scale == 2);
        CHECK(uqa_value_destroy(&value));
    }
    {
        const char *types[] = {"character(4)", "varchar(2)", "integer[]", "int4range", "int4multirange"};
        const char *inputs[] = {"ab", "abcd", "[0:1]={7,NULL}", "(1,4]", "{[1,3),[3,5)}"};
        const char *outputs[] = {"ab  ", "ab", "[0:1]={7,NULL}", "[2,5)", "{[1,5)}"};
        for (size_t index = 0; index < sizeof(types) / sizeof(types[0]); ++index) {
            CHECK(uqa_value_create_from_text(engine, types[index], inputs[index], &value));
            formatted(value, buffer, outputs[index]);
            CHECK(uqa_value_destroy(&value));
        }
    }
    value = (uqa_value)(uintptr_t)1;
    assert(uqa_value_create_typed_null(engine, "integer", &value) == UQA_ERROR_INVALID_ARGUMENT);
    assert(value == (uqa_value)(uintptr_t)1);
    value = NULL;
    sql_error(uqa_value_create_from_text(engine, "smallint", "32768", &value), "22003");
    assert(value == NULL);
    sql_error(uqa_value_create_typed_null(engine, "missing_host_type", &value), "42704");
    assert(value == NULL);
    assert(uqa_value_create_from_text(engine, "integer", NULL, &value) == UQA_ERROR_INVALID_ARGUMENT);
    assert(value == NULL);
    assert(uqa_value_create_typed_null(NULL, "integer", &value) == UQA_ERROR_INVALID_ARGUMENT);
    {
        const char invalid[] = {(char)0xff};
        assert(uqa_value_create_typed_null_n(engine, invalid, 1, &value) == UQA_ERROR_INVALID_ARGUMENT);
        assert(value == NULL);
    }
}

static void domains(uqa_engine engine, uqa_result result) {
    uqa_value value = NULL;
    uint32_t state = UQA_TRANSACTION_IDLE;
    CHECK(uqa_engine_execute(engine,
        "CREATE SCHEMA c_host_input_ref; "
        "CREATE TABLE c_host_input_ref.audit(n integer); "
        "CREATE FUNCTION c_host_input_ref.check_value(p integer) RETURNS boolean "
        "LANGUAGE plpgsql VOLATILE AS $$ BEGIN INSERT INTO c_host_input_ref.audit VALUES (p); "
        "RETURN p > 0; END $$; "
        "CREATE DOMAIN c_host_input_ref.audited AS integer CHECK (c_host_input_ref.check_value(VALUE)); "
        "CREATE DOMAIN c_host_input_ref.required AS integer NOT NULL", result));
    CHECK(uqa_value_create_from_text(engine, "c_host_input_ref.audited", "11", &value));
    {
        uqa_value_view params[] = {value};
        CHECK(uqa_engine_execute_params(engine, "SELECT $1", params, 1, result));
        CHECK(uqa_engine_execute_params(engine, "SELECT $1", params, 1, result));
    }
    CHECK(uqa_value_destroy(&value));
    assert(count(engine, result) == 1);
    sql_error(uqa_value_create_from_text(engine, "c_host_input_ref.audited", "-1", &value), "23514");
    assert(value == NULL);
    assert(count(engine, result) == 1);
    sql_error(uqa_value_create_typed_null(engine, "c_host_input_ref.required", &value), "23502");
    CHECK(uqa_engine_execute(engine, "BEGIN; SAVEPOINT input_retry", result));
    CHECK(uqa_value_create_from_text(engine, "c_host_input_ref.audited", "12", &value));
    CHECK(uqa_value_destroy(&value));
    sql_error(uqa_value_create_from_text(engine, "c_host_input_ref.audited", "-2", &value), "23514");
    CHECK(uqa_transaction_get_state(engine, &state));
    assert(state == UQA_TRANSACTION_FAILED);
    sql_error(uqa_value_create_from_text(engine, "c_host_input_ref.audited", "13", &value), "25P02");
    CHECK(uqa_engine_execute(engine, "ROLLBACK TO input_retry", result));
    assert(count(engine, result) == 1);
    CHECK(uqa_value_create_from_text(engine, "c_host_input_ref.audited", "14", &value));
    CHECK(uqa_value_destroy(&value));
    CHECK(uqa_engine_execute(engine, "ROLLBACK", result));
    assert(count(engine, result) == 1);
}

static void catalog_values(uqa_engine engine, uqa_result result, uqa_buffer buffer) {
    uqa_engine sibling = NULL;
    uqa_engine source = engine;
    uqa_engine other = NULL;
    uqa_value value = NULL;
    uqa_value copy = NULL;
    CHECK(uqa_engine_execute(engine,
        "CREATE TYPE c_host_input_ref.mood AS ENUM ('calm', 'busy'); "
        "CREATE TYPE c_host_input_ref.pair AS (n integer, label text)", result));
    if (test_database_flags() == UQA_OPEN_MEMORY) {
        assert(uqa_engine_open_session(engine, &sibling) == UQA_ERROR_UNSUPPORTED);
        assert(sibling == NULL);
    } else {
        CHECK(uqa_engine_open_session(engine, &sibling));
        source = sibling;
    }
    CHECK(uqa_value_create_from_text(source, "c_host_input_ref.mood", "calm", &value));
    CHECK(uqa_value_create_copy(value, &copy));
    CHECK(uqa_value_destroy(&value));
    CHECK(uqa_engine_execute(source,
        "ALTER TYPE c_host_input_ref.mood RENAME VALUE 'calm' TO 'quiet'", result));
    formatted(copy, buffer, "quiet");
    {
        uqa_value_view params[] = {copy};
        uqa_value_view cell = NULL;
        CHECK(uqa_engine_execute_params(engine, "SELECT $1", params, 1, result));
        CHECK(uqa_result_get_value(result, 0, 0, &cell));
        formatted(cell, buffer, "quiet");
        CHECK(test_database_open(NULL, &other));
        assert(uqa_engine_execute_params(other, "SELECT $1", params, 1, result) == UQA_ERROR_TYPE);
        CHECK(uqa_engine_close(&other));
    }
    CHECK(uqa_value_destroy(&copy));
    CHECK(uqa_value_create_from_text(engine, "c_host_input_ref.pair", "(,)", &value));
    {
        uint8_t is_null = 1;
        CHECK(uqa_value_get_is_null(value, &is_null));
        assert(is_null == 0);
        formatted(value, buffer, "(,)");
    }
    CHECK(uqa_value_destroy(&value));
    CHECK(uqa_value_create_typed_null(engine, "c_host_input_ref.pair", &value));
    {
        uint8_t is_null = 0;
        CHECK(uqa_value_get_is_null(value, &is_null));
        assert(is_null == 1);
    }
    CHECK(uqa_value_destroy(&value));
    if (sibling != NULL) CHECK(uqa_engine_close(&sibling));
}

static void restored_input(void) {
    uqa_engine engine = NULL;
    uqa_result result = NULL;
    uqa_buffer buffer = NULL;
    uqa_value value = NULL;
    char path[4096];
    int length;
    if (test_database_flags() == UQA_OPEN_MEMORY) return;
    length = snprintf(path, sizeof(path), "%s/restored-input", getenv("UQA_TEST_DATABASE_DIR"));
    assert(length > 0 && (size_t)length < sizeof(path));
    CHECK(test_database_open_path(path, NULL, &engine));
    CHECK(uqa_result_create(&result));
    CHECK(uqa_buffer_create(&buffer));
    CHECK(uqa_engine_execute(engine,
        "CREATE TYPE restored_mood AS ENUM ('calm'); "
        "CREATE DOMAIN restored_positive AS integer CHECK (VALUE > 0); "
        "CREATE TABLE restored_values(mood restored_mood, n restored_positive)", result));
    CHECK(uqa_value_create_from_text(engine, "restored_mood", "calm", &value));
    {
        uqa_value_view params[] = {value};
        CHECK(uqa_engine_execute_params(engine, "INSERT INTO restored_values VALUES ($1, 7)", params, 1, result));
    }
    CHECK(uqa_value_destroy(&value));
    CHECK(uqa_result_destroy(&result));
    CHECK(uqa_engine_close(&engine));
    CHECK(test_database_open_path(path, NULL, &engine));
    CHECK(uqa_result_create(&result));
    CHECK(uqa_value_create_from_text(engine, "restored_mood", "calm", &value));
    formatted(value, buffer, "calm");
    {
        uqa_value_view params[] = {value};
        uqa_value_view cell = NULL;
        int32_t number = 0;
        CHECK(uqa_engine_execute_params(engine, "SELECT n FROM restored_values WHERE mood = $1", params, 1, result));
        CHECK(uqa_result_get_value(result, 0, 0, &cell));
        CHECK(uqa_value_get_int32(cell, &number));
        assert(number == 7);
    }
    CHECK(uqa_value_destroy(&value));
    sql_error(uqa_value_create_from_text(engine, "restored_positive", "0", &value), "23514");
    CHECK(uqa_buffer_destroy(&buffer));
    CHECK(uqa_result_destroy(&result));
    CHECK(uqa_engine_close(&engine));
}

/* The independent declaration/transaction oracle is parameters/live.pg18.json in the Engine owner's tests. These clients exercise the actual ABI paths. */
static void live_types(uqa_engine source) {
    const char *types[] = {
        "c_live_host.mood", "c_live_host.mood[]",
        "c_live_host.amount", "c_live_host.amount[]",
        "c_live_host.pair", "c_live_host.pair[]",
        "c_live_host.row_value", "c_live_host.row_value[]"
    };
    const char *queries[] = {"SELECT $1 IS NULL", "SELECT 1 AS cached", "", "COMMIT", "ROLLBACK", "BEGIN"};
    uqa_value values[8] = {NULL};
    uqa_value untyped_null = NULL;
    uqa_statement declared[8][2] = {{NULL}};
    uqa_result result = NULL;
    uqa_engine sibling = NULL;
    uqa_engine engine = source;
    uqa_statement statement = NULL;
    uint32_t state = UQA_TRANSACTION_IDLE;
    CHECK(uqa_result_create(&result));
    if (test_database_flags() != UQA_OPEN_MEMORY) {
        CHECK(uqa_engine_open_session(source, &sibling));
        engine = sibling;
    }
    CHECK(uqa_engine_execute(source,
        "CREATE SCHEMA c_live_host; "
        "CREATE TYPE c_live_host.mood AS ENUM ('calm'); "
        "CREATE DOMAIN c_live_host.amount AS integer; "
        "CREATE TYPE c_live_host.pair AS (n integer); "
        "CREATE TABLE c_live_host.row_value(n integer)", result));
    CHECK(uqa_statement_prepare(engine, "SELECT $1::text", &statement));
    CHECK(uqa_value_create_null(&untyped_null));
    for (size_t index = 0; index < 8; ++index) {
        const char *target[] = {types[index]};
        CHECK(uqa_value_create_typed_null(source, types[index], &values[index]));
        CHECK(uqa_statement_prepare_typed(engine, "SELECT $1 IS NULL", target, 1, &declared[index][0]));
        CHECK(uqa_statement_prepare_typed(engine, "SELECT 1 AS value", target, 1, &declared[index][1]));
    }
    CHECK(uqa_engine_execute(source,
        "DROP TYPE c_live_host.mood; DROP DOMAIN c_live_host.amount; "
        "DROP TYPE c_live_host.pair; DROP TABLE c_live_host.row_value; "
        "CREATE TYPE c_live_host.mood AS ENUM ('replacement')", result));
    CHECK(uqa_engine_execute(engine, "SELECT 1 AS cached", result));
    for (size_t index = 0; index < 8; ++index) {
        uqa_value copy = NULL;
        uqa_value_view params[] = {values[index]};
        uqa_type type = NULL;
        uint32_t oid = 0;
        char expected[80];
        CHECK(uqa_value_get_type(values[index], &type));
        CHECK(uqa_type_get_oid(type, &oid));
        (void)snprintf(expected, sizeof(expected), "cache lookup failed for type %u", (unsigned)oid);
        /* Copy and metadata access do not bind or observe a retained payload. */
        CHECK(uqa_value_create_copy(values[index], &copy));
        CHECK(uqa_value_destroy(&copy));
        for (size_t query = 0; query < sizeof(queries) / sizeof(queries[0]); ++query) {
            uqa_error error = NULL;
            uqa_string_view message = {NULL, 0};
            uint64_t rows = 99;
            sql_error(uqa_engine_execute_params(engine, queries[query], params, 1, result), "XX000");
            CHECK(uqa_error_get_last(&error));
            CHECK(uqa_error_get_message(error, &message));
            assert(equal(message, expected));
            CHECK(uqa_result_get_row_count(result, &rows));
            assert(rows == 1);
            {
                uqa_value_view retained = NULL;
                int32_t number = 0;
                CHECK(uqa_result_get_value(result, 0, 0, &retained));
                CHECK(uqa_value_get_int32(retained, &number));
                assert(number == 1);
            }
            CHECK(uqa_transaction_get_state(engine, &state));
            assert(state == UQA_TRANSACTION_IDLE);
        }
        {
            uqa_cursor cursor = NULL;
            sql_error(uqa_cursor_open_params(engine, "SELECT 1 AS cached", params, 1, &cursor), "XX000");
            assert(cursor == NULL);
        }
        CHECK(uqa_statement_bind(statement, 1, values[index]));
        sql_error(uqa_statement_execute(statement, result), "XX000");
        for (size_t target = 0; target < 2; ++target) {
            CHECK(uqa_statement_bind(declared[index][target], 1, untyped_null));
            sql_error(uqa_statement_execute(declared[index][target], result), "XX000");
            CHECK(uqa_statement_finalize(&declared[index][target]));
        }
        CHECK(uqa_value_destroy(&values[index]));
    }
    CHECK(uqa_value_destroy(&untyped_null));
    CHECK(uqa_statement_finalize(&statement));
    {
        uqa_value value = NULL;
        CHECK(uqa_engine_execute(engine, "CREATE TYPE c_live_host.restored AS ENUM ('calm')", result));
        CHECK(uqa_value_create_typed_null(engine, "c_live_host.restored", &value));
        {
            uqa_value_view params[] = {value};
            CHECK(uqa_engine_execute(engine, "BEGIN; SAVEPOINT type_keep; DROP TYPE c_live_host.restored", result));
            sql_error(uqa_engine_execute_params(engine, "SELECT $1 IS NULL", params, 1, result), "XX000");
            CHECK(uqa_transaction_get_state(engine, &state));
            assert(state == UQA_TRANSACTION_FAILED);
            sql_error(uqa_engine_execute_params(engine, "ROLLBACK TO type_keep", params, 1, result), "25P02");
            CHECK(uqa_engine_execute(engine, "ROLLBACK TO type_keep", result));
            CHECK(uqa_engine_execute_params(engine, "SELECT $1 IS NULL", params, 1, result));
            CHECK(uqa_engine_execute_params(engine, "COMMIT", params, 1, result));
        }
        CHECK(uqa_value_destroy(&value));
        CHECK(uqa_engine_execute(engine,
            "BEGIN; CREATE TYPE c_live_host.private_type AS ENUM ('calm')", result));
        CHECK(uqa_value_create_typed_null(engine, "c_live_host.private_type", &value));
        CHECK(uqa_engine_execute(engine, "ROLLBACK", result));
        {
            uqa_value_view params[] = {value};
            sql_error(uqa_engine_execute_params(engine, "SELECT 1 AS cached", params, 1, result), "XX000");
        }
        CHECK(uqa_value_destroy(&value));
        CHECK(uqa_value_create_typed_null(engine, "integer", &value));
        {
            uqa_value_view params[] = {value};
            CHECK(uqa_engine_execute_params(engine, "", params, 1, result));
        }
        CHECK(uqa_engine_execute(engine, "BEGIN; SAVEPOINT failed_keep", result));
        sql_error(uqa_engine_execute(engine, "SELECT 1 / 0", result), "22012");
        CHECK(uqa_engine_execute(engine, "", result));
        CHECK(uqa_transaction_get_state(engine, &state));
        assert(state == UQA_TRANSACTION_FAILED);
        {
            uqa_value_view params[] = {value};
            const char *failed[] = {"", "SELECT 1 AS cached", "ROLLBACK", "COMMIT", "ROLLBACK TO failed_keep"};
            for (size_t index = 0; index < sizeof(failed) / sizeof(failed[0]); ++index) {
                sql_error(uqa_engine_execute_params(engine, failed[index], params, 1, result), "25P02");
                CHECK(uqa_transaction_get_state(engine, &state));
                assert(state == UQA_TRANSACTION_FAILED);
            }
            CHECK(uqa_engine_execute(engine, "ROLLBACK TO failed_keep", result));
            CHECK(uqa_engine_execute_params(engine, "ROLLBACK", params, 1, result));
        }
        CHECK(uqa_value_destroy(&value));
    }
    CHECK(uqa_engine_execute(engine, "DROP SCHEMA c_live_host CASCADE", result));
    CHECK(uqa_result_destroy(&result));
    if (sibling != NULL) CHECK(uqa_engine_close(&sibling));
}

int main(void) {
    uqa_engine engine = NULL;
    uqa_result result = NULL;
    uqa_buffer buffer = NULL;
    uqa_value lease = NULL;
    CHECK(test_database_open(NULL, &engine));
    CHECK(uqa_result_create(&result));
    CHECK(uqa_buffer_create(&buffer));
    builtins(engine, result, buffer);
    domains(engine, result);
    catalog_values(engine, result, buffer);
    live_types(engine);
    CHECK(uqa_value_create_typed_null(engine, "integer", &lease));
    CHECK(uqa_buffer_destroy(&buffer));
    CHECK(uqa_result_destroy(&result));
    assert(uqa_engine_close(&engine) == UQA_ERROR_BUSY && engine != NULL);
    CHECK(uqa_value_destroy(&lease));
    CHECK(uqa_engine_close(&engine));
    restored_input();
    return 0;
}
