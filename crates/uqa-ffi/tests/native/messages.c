/*
 * Unified Query Algebra
 *
 * Copyright (c) 2023-2026 Cognica, Inc.
 */

#include <uqa/uqa.h>
#include <uqa/uqa_arrow.h>
#include <uqa/uqa_log.h>
#include <uqa/uqa_udf.h>
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

static int equal(uqa_string_view value, const char *text) {
    size_t size = strlen(text);
    return value.size == size && (size == 0 || memcmp(value.data, text, size) == 0);
}

static int64_t integer(uqa_result result, uint32_t column) {
    uqa_value_view value = NULL;
    int64_t number = 0;
    CHECK(uqa_result_get_value(result, 0, column, &value));
    CHECK(uqa_value_get_int64(value, &number));
    return number;
}

int main(void) {
    uqa_engine engine = NULL;
    uqa_result result = NULL;
    uqa_batch_result batch = NULL;
    uqa_value copy = NULL;
    uqa_cancel_handle cancel = NULL;
    uqa_value_view view = NULL;
    uqa_type type = NULL;
    uqa_error error = NULL;
    uqa_string_view text = {NULL, 0};
    uint64_t count = 99;
    uint32_t columns = 99;
    uint32_t state = 99;

    CHECK(uqa_engine_open_memory(NULL, &engine));
    CHECK(uqa_result_create(&result));
    assert(uqa_result_create(&result) == UQA_ERROR_INVALID_ARGUMENT);
    assert(uqa_result_get_row_count(result, &count) == UQA_ERROR_STATE);
    assert(count == 0);
    CHECK(uqa_engine_execute(engine,
        "CREATE TABLE items (n integer); INSERT INTO items VALUES (1); "
        "SELECT n AS x, n + 1 AS x FROM items", result));
    CHECK(uqa_result_get_row_count(result, &count));
    CHECK(uqa_result_get_column_count(result, &columns));
    assert(count == 1 && columns == 2);
    CHECK(uqa_result_get_column_name(result, 0, &text));
    assert(equal(text, "x"));
    CHECK(uqa_result_get_column_name(result, 1, &text));
    assert(equal(text, "x"));
    assert(integer(result, 0) == 1 && integer(result, 1) == 2);
    CHECK(uqa_result_get_command_tag(result, &text));
    assert(equal(text, "SELECT 1"));
    CHECK(uqa_result_get_column_type(result, 0, &type));
    CHECK(uqa_type_get_kind(type, &state));
    assert(state == UQA_TYPE_INT32);
    CHECK(uqa_type_get_oid(type, &state));
    assert(state == 23);
    CHECK(uqa_result_get_value(result, 0, 0, &view));
    CHECK(uqa_value_create_copy(view, &copy));
    assert(uqa_value_create_null(&copy) == UQA_ERROR_INVALID_ARGUMENT);
    assert(uqa_engine_close(&engine) == UQA_ERROR_BUSY && engine != NULL);

    /* An input may borrow the result being replaced; it must be copied before refill. */
    {
        uqa_value_view params[] = {view};
        const char sql[] = "SELECT $1 + 40; SELECT $1 + 2";
        CHECK(uqa_engine_execute_params_n(engine, sql, sizeof(sql) - 1, params, 1, result));
        assert(integer(result, 0) == 3);
    }
    CHECK(uqa_engine_execute(engine, "SELECT n FROM items WHERE false", result));
    CHECK(uqa_result_get_row_count(result, &count));
    assert(count == 0);
    CHECK(uqa_result_get_column_type(result, 0, &type));
    CHECK(uqa_type_get_oid(type, &state));
    assert(state == 23);
    CHECK(uqa_engine_execute_n(engine, NULL, 0, result));
    CHECK(uqa_result_get_row_count(result, &count));
    assert(count == 0);
    assert(uqa_result_get_command_tag(result, &text) == UQA_DONE);
    assert(text.data == NULL && text.size == 0);

    CHECK(uqa_engine_execute(engine, "SELECT 900", result));
    assert(uqa_engine_execute(engine, "INSERT INTO items VALUES (2); SELECT 1 / 0", result) == UQA_ERROR_SQL);
    CHECK(uqa_error_get_last(&error));
    CHECK(uqa_error_get_sqlstate(error, &text));
    assert(equal(text, "22012"));
    assert(integer(result, 0) == 900);
    CHECK(uqa_engine_execute(engine, "SELECT count(*) FROM items", result));
    assert(integer(result, 0) == 1);

    /* An explicit commit preserves an earlier segment when a later segment fails. */
    assert(uqa_engine_execute(engine,
        "INSERT INTO items VALUES (3); COMMIT; INSERT INTO items VALUES (4); SELECT 1 / 0",
        result) == UQA_ERROR_SQL);
    CHECK(uqa_engine_execute(engine, "SELECT sum(n) FROM items", result));
    assert(integer(result, 0) == 4);

    CHECK(uqa_batch_result_create(&batch));
    {
        const uqa_sql_request requests[] = {
            {"INSERT INTO items VALUES (10); SELECT 10", NULL, 0},
            {"INSERT INTO items VALUES (20); SELECT 20", NULL, 0}
        };
        CHECK(uqa_engine_execute_batch(engine, requests, 2, batch));
        CHECK(uqa_batch_result_get_count(batch, &count));
        assert(count == 2);
        CHECK(uqa_batch_result_take(batch, 0, result));
        assert(integer(result, 0) == 10);
        assert(uqa_batch_result_take(batch, 0, result) == UQA_ERROR_STATE);
        assert(integer(result, 0) == 10);
    }
    {
        const uqa_sql_request requests[] = {
            {"INSERT INTO items VALUES (30); SELECT 30", NULL, 0},
            {"SELECT * FROM missing_relation", NULL, 0}
        };
        assert(uqa_engine_execute_batch(engine, requests, 2, batch) == UQA_ERROR_SQL);
        CHECK(uqa_error_get_last(&error));
        CHECK(uqa_error_get_statement_index(error, &count));
        assert(count == 1);
        CHECK(uqa_batch_result_take(batch, 1, result));
        assert(integer(result, 0) == 20);
        CHECK(uqa_engine_execute(engine, "SELECT sum(n) FROM items", result));
        assert(integer(result, 0) == 34);
    }

    CHECK(uqa_transaction_begin(engine));
    {
        const uqa_value_view invalid[] = {NULL};
        assert(uqa_engine_execute_params(engine, "SELECT $1", invalid, 1, result) == UQA_ERROR_INVALID_ARGUMENT);
        CHECK(uqa_transaction_get_state(engine, &state));
        assert(state == UQA_TRANSACTION_ACTIVE);
    }
    CHECK(uqa_transaction_rollback(engine));
    CHECK(uqa_cancel_handle_create(engine, &cancel));
    CHECK(uqa_cancel_handle_request(cancel));
    assert(uqa_engine_execute(engine, "SELECT 1", result) == UQA_ERROR_SQL);
    CHECK(uqa_error_get_last(&error));
    CHECK(uqa_error_get_sqlstate(error, &text));
    assert(equal(text, "57014"));
    CHECK(uqa_engine_reset_cancellation(engine));
    CHECK(uqa_engine_execute(engine, "SELECT 1", result));

    {
        uqa_buffer buffer = NULL;
        uqa_bytes_view bytes = {NULL, 0};
        int64_t number = 99;
        CHECK(uqa_buffer_create(&buffer));
        CHECK(uqa_engine_execute(engine, "SELECT NULL::integer, 'abc'::char(5), '[1,3)'::int4range, INTERVAL '1 day'", result));
        CHECK(uqa_result_get_value(result, 0, 0, &view));
        assert(uqa_value_get_int64(view, &number) == UQA_ERROR_NULL && number == 0);
        CHECK(uqa_result_get_value(result, 0, 1, &view));
        CHECK(uqa_value_get_text(view, &text));
        assert(equal(text, "abc  "));
        CHECK(uqa_result_get_value(result, 0, 2, &view));
        assert(uqa_value_get_text(view, &text) == UQA_ERROR_TYPE);
        assert(text.data == NULL && text.size == 0);
        CHECK(uqa_value_format_sql(view, buffer));
        CHECK(uqa_buffer_get_bytes(buffer, &bytes));
        assert(bytes.size == 5 && memcmp(bytes.data, "[1,3)", 5) == 0);
        CHECK(uqa_result_get_value(result, 0, 3, &view));
        {
            uqa_result settings = NULL;
            CHECK(uqa_result_create(&settings));
            CHECK(uqa_engine_execute(engine, "SET IntervalStyle = 'iso_8601'", settings));
            CHECK(uqa_result_destroy(&settings));
        }
        CHECK(uqa_value_format_sql(view, buffer));
        CHECK(uqa_buffer_get_bytes(buffer, &bytes));
        assert(bytes.size == 3 && memcmp(bytes.data, "P1D", 3) == 0);
        CHECK(uqa_buffer_destroy(&buffer));
    }

    CHECK(uqa_result_destroy(&result));
    CHECK(uqa_batch_result_destroy(&batch));
    assert(uqa_engine_close(&engine) == UQA_ERROR_BUSY && engine != NULL);
    CHECK(uqa_value_destroy(&copy));
    CHECK(uqa_engine_close(&engine));
    assert(engine == NULL);
    /* Cancellation handles own their token independently of session lifetime. */
    CHECK(uqa_cancel_handle_request(cancel));
    CHECK(uqa_cancel_handle_destroy(&cancel));
    CHECK(uqa_engine_close(&engine));
    CHECK(uqa_result_destroy(&result));
    return 0;
}
