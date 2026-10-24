/*
 * Unified Query Algebra
 *
 * Copyright (c) 2023-2026 Cognica, Inc.
 */

#include <uqa/uqa.h>
#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

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

static int equal(uqa_string_view text, const char *expected) {
    return text.size == strlen(expected) && memcmp(text.data, expected, text.size) == 0;
}

static int64_t integer(uqa_result result, uint32_t column) {
    uqa_value_view value = NULL;
    int64_t number = 0;
    CHECK(uqa_result_get_value(result, 0, column, &value));
    CHECK(uqa_value_get_int64(value, &number));
    return number;
}

static void sqlstate(const char *expected) {
    uqa_error error = NULL;
    uqa_string_view state = {NULL, 0};
    CHECK(uqa_error_get_last(&error));
    CHECK(uqa_error_get_sqlstate(error, &state));
    assert(equal(state, expected));
}

static void empty_result(uqa_result result) {
    uint64_t rows = 99;
    uint32_t columns = 99;
    uqa_string_view tag = {NULL, 0};
    CHECK(uqa_result_get_row_count(result, &rows));
    CHECK(uqa_result_get_column_count(result, &columns));
    assert(rows == 0 && columns == 0);
    assert(uqa_result_get_command_tag(result, &tag) == UQA_DONE);
    assert(tag.data == NULL && tag.size == 0);
}

int main(void) {
    uqa_engine engine = NULL;
    uqa_result result = NULL;
    uqa_statement statement = NULL;
    uqa_value value = NULL;
    uqa_value_view view = NULL;
    uqa_type type = NULL;
    uqa_string_view name = {NULL, 0};
    uint32_t count = 99;
    uint32_t kind = 0;
    uint8_t is_null = 0;
    uint64_t rows = 99;

    CHECK(uqa_engine_open_memory(NULL, &engine));
    CHECK(uqa_result_create(&result));
    CHECK(uqa_engine_execute(engine, "SELECT 81", result));
    CHECK(uqa_statement_prepare(engine, "SELECT $1::integer AS x, $1 + 1 AS x", &statement));
    CHECK(uqa_statement_get_parameter_count(statement, &count));
    assert(count == 1);
    CHECK(uqa_statement_get_parameter_type(statement, 1, &type));
    CHECK(uqa_type_get_kind(type, &kind));
    assert(kind == UQA_TYPE_INT32);
    CHECK(uqa_statement_get_column_count(statement, &count));
    assert(count == 2);
    CHECK(uqa_statement_get_column_name(statement, 0, &name));
    assert(equal(name, "x"));
    CHECK(uqa_statement_get_column_name(statement, 1, &name));
    assert(equal(name, "x"));
    CHECK(uqa_statement_get_column_type(statement, 0, &type));
    CHECK(uqa_type_get_kind(type, &kind));
    assert(kind == UQA_TYPE_INT32);
    assert(uqa_statement_get_parameter_type(statement, 0, &type) == UQA_ERROR_RANGE);
    assert(type == NULL);
    assert(uqa_statement_execute(statement, result) == UQA_ERROR_STATE);
    assert(integer(result, 0) == 81);
    CHECK(uqa_value_create_int64(7, &value));
    CHECK(uqa_statement_bind(statement, 1, value));
    CHECK(uqa_value_destroy(&value));
    assert(uqa_statement_bind(statement, 1, NULL) == UQA_ERROR_INVALID_ARGUMENT);
    CHECK(uqa_statement_execute(statement, result));
    assert(integer(result, 0) == 7 && integer(result, 1) == 8);
    CHECK(uqa_statement_execute(statement, result));
    assert(integer(result, 0) == 7);
    CHECK(uqa_statement_clear_bindings(statement));
    assert(uqa_statement_execute(statement, result) == UQA_ERROR_STATE);
    assert(integer(result, 0) == 7);
    CHECK(uqa_value_create_null(&value));
    CHECK(uqa_statement_bind(statement, 1, value));
    CHECK(uqa_value_destroy(&value));
    CHECK(uqa_statement_execute(statement, result));
    CHECK(uqa_result_get_value(result, 0, 0, &view));
    CHECK(uqa_value_get_is_null(view, &is_null));
    assert(is_null);
    assert(uqa_engine_close(&engine) == UQA_ERROR_BUSY);
    CHECK(uqa_statement_finalize(&statement));
    CHECK(uqa_value_get_is_null(view, &is_null));
    assert(is_null);

    /* Explicit declarations preserve native typing even for a NULL value. */
    {
        const char *types[] = {"bigint"};
        CHECK(uqa_statement_prepare_typed(engine, "SELECT $1 AS n", types, 1, &statement));
        CHECK(uqa_statement_get_parameter_type(statement, 1, &type));
        CHECK(uqa_type_get_kind(type, &kind));
        assert(kind == UQA_TYPE_INT64);
        CHECK(uqa_value_create_null(&value));
        CHECK(uqa_statement_bind(statement, 1, value));
        CHECK(uqa_value_destroy(&value));
        CHECK(uqa_statement_execute(statement, result));
        CHECK(uqa_result_get_column_type(result, 0, &type));
        CHECK(uqa_type_get_kind(type, &kind));
        assert(kind == UQA_TYPE_INT64);
        CHECK(uqa_statement_finalize(&statement));
    }
    {
        const char query[] = "SELECT $1::integer WHERE false trailing";
        const char type_name[] = {'i', 'n', 't'};
        const char *types[] = {type_name};
        const size_t sizes[] = {sizeof(type_name)};
        CHECK(uqa_statement_prepare_typed_n(engine, query, strlen("SELECT $1::integer WHERE false"),
                                             types, sizes, 1, &statement));
        CHECK(uqa_value_create_int64(3, &value));
        CHECK(uqa_statement_bind(statement, 1, value));
        CHECK(uqa_value_destroy(&value));
        CHECK(uqa_statement_execute(statement, result));
        CHECK(uqa_result_get_row_count(result, &rows));
        assert(rows == 0);
        CHECK(uqa_statement_get_column_count(statement, &count));
        assert(count == 1);
        CHECK(uqa_statement_finalize(&statement));
    }

    CHECK(uqa_engine_execute(engine, "CREATE TABLE items (n integer)", result));
    CHECK(uqa_statement_prepare_n(engine, "INSERT INTO items VALUES ($1) RETURNING n",
                                  strlen("INSERT INTO items VALUES ($1) RETURNING n"), &statement));
    CHECK(uqa_value_create_int64(9, &value));
    CHECK(uqa_statement_bind(statement, 1, value));
    CHECK(uqa_value_destroy(&value));
    CHECK(uqa_engine_execute(engine, "BEGIN", result));
    CHECK(uqa_statement_execute(statement, result));
    assert(integer(result, 0) == 9);
    CHECK(uqa_engine_execute(engine, "ROLLBACK", result));
    CHECK(uqa_statement_execute(statement, result));
    assert(integer(result, 0) == 9);
    CHECK(uqa_statement_finalize(&statement));
    CHECK(uqa_engine_execute(engine, "SELECT count(*) FROM items", result));
    assert(integer(result, 0) == 1);

    CHECK(uqa_statement_prepare(engine, "SELECT * FROM items", &statement));
    CHECK(uqa_engine_execute(engine, "ALTER TABLE items ADD COLUMN extra text", result));
    assert(uqa_statement_execute(statement, result) == UQA_ERROR_SQL);
    sqlstate("0A000");
    CHECK(uqa_statement_finalize(&statement));

    /* A SQL user can discover a host definition's name, but replacing that name
       cannot make an older C handle target the new definition. */
    CHECK(uqa_statement_prepare(engine, "SELECT 33 AS saved", &statement));
    CHECK(uqa_engine_execute(engine, "SELECT name FROM pg_prepared_statements", result));
    CHECK(uqa_result_get_value(result, 0, 0, &view));
    CHECK(uqa_value_get_text(view, &name));
    {
        char commands[256];
        int size = snprintf(commands, sizeof(commands), "DEALLOCATE \"%.*s\"; PREPARE \"%.*s\" AS SELECT 44",
                            (int)name.size, name.data, (int)name.size, name.data);
        assert(size > 0 && (size_t)size < sizeof(commands));
        CHECK(uqa_engine_execute(engine, commands, result));
    }
    assert(uqa_statement_get_parameter_count(statement, &count) == UQA_ERROR_SQL);
    assert(count == 0);
    sqlstate("26000");
    assert(uqa_statement_execute(statement, result) == UQA_ERROR_SQL);
    CHECK(uqa_statement_finalize(&statement));
    CHECK(uqa_engine_execute(engine, "SELECT count(*) FROM pg_prepared_statements", result));
    assert(integer(result, 0) == 1);
    CHECK(uqa_engine_execute(engine, "DEALLOCATE ALL", result));

    CHECK(uqa_statement_prepare(engine, "SELECT 55", &statement));
    CHECK(uqa_engine_execute(engine, "DISCARD ALL", result));
    assert(uqa_statement_execute(statement, result) == UQA_ERROR_SQL);
    sqlstate("26000");
    CHECK(uqa_statement_finalize(&statement));
    CHECK(uqa_statement_finalize(&statement));
    assert(uqa_statement_prepare(engine, "SELECT 1; SELECT 2", &statement) == UQA_ERROR_SQL);
    assert(statement == NULL);

    CHECK(uqa_engine_execute(engine, "BEGIN", result));
    CHECK(uqa_statement_prepare(engine, " ; -- empty\n; ", &statement));
    CHECK(uqa_statement_get_parameter_count(statement, &count));
    assert(count == 0);
    CHECK(uqa_statement_get_column_count(statement, &count));
    assert(count == 0);
    CHECK(uqa_statement_execute(statement, result));
    empty_result(result);
    CHECK(uqa_engine_execute(engine, "SET TRANSACTION ISOLATION LEVEL SERIALIZABLE", result));
    CHECK(uqa_engine_execute(engine, "SAVEPOINT keep", result));
    assert(uqa_engine_execute(engine, "SELECT 1/0", result) == UQA_ERROR_SQL);
    sqlstate("22012");
    assert(uqa_statement_execute(statement, result) == UQA_ERROR_SQL);
    sqlstate("25P02");
    CHECK(uqa_statement_finalize(&statement));
    CHECK(uqa_statement_prepare_n(engine, "ignored", 0, &statement));
    CHECK(uqa_statement_get_column_count(statement, &count));
    assert(count == 0);
    assert(uqa_statement_execute(statement, result) == UQA_ERROR_SQL);
    sqlstate("25P02");
    CHECK(uqa_engine_execute(engine, "ROLLBACK TO keep", result));
    CHECK(uqa_statement_execute(statement, result));
    empty_result(result);
    CHECK(uqa_statement_finalize(&statement));
    CHECK(uqa_engine_execute(engine, "COMMIT", result));

    {
        const char *types[] = {"integer"};
        CHECK(uqa_statement_prepare_typed(engine, "", types, 1, &statement));
        CHECK(uqa_statement_get_parameter_type(statement, 1, &type));
        CHECK(uqa_type_get_kind(type, &kind));
        assert(kind == UQA_TYPE_INT32);
        assert(uqa_statement_execute(statement, result) == UQA_ERROR_STATE);
        CHECK(uqa_value_create_int64(4, &value));
        CHECK(uqa_statement_bind(statement, 1, value));
        CHECK(uqa_value_destroy(&value));
        CHECK(uqa_statement_execute(statement, result));
        empty_result(result);
        CHECK(uqa_statement_finalize(&statement));
    }
    CHECK(uqa_result_destroy(&result));
    CHECK(uqa_engine_close(&engine));
    return 0;
}
