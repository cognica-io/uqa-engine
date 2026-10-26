/*
 * Unified Query Algebra
 *
 * Copyright (c) 2023-2026 Cognica, Inc.
 */

#include "test_udf.h"

enum mode { ROWS, CREATE_FAIL, NEXT_FAIL, NO_ROW, DONE_ROW, RETRY_EMIT, CANCEL, ENUM_ARGUMENT };
struct data {
    enum mode mode;
    int created, destroyed, next, released, fail_destroy;
    uqa_cancel_handle cancellation;
    const char *label;
};
struct scan { int64_t current, count; };

static uqa_status UQA_CALL create_scan(const uqa_value_view *arguments, uint32_t count,
    void *user_data, void **out_scan) {
    struct data *data = (struct data *)user_data;
    assert(count == 1);
    if (data->mode == CREATE_FAIL) return uqa_callback_fail(UQA_ERROR_IO, "scan creation failed");
    struct scan *scan = (struct scan *)malloc(sizeof(*scan));
    assert(scan != NULL);
    scan->current = 0;
    if (data->mode == ENUM_ARGUMENT) {
        uqa_string_view text = {NULL, 0};
        CHECK(uqa_value_get_text(arguments[0], &text));
        assert(udf_equal(text, data->label));
        scan->count = 1;
    } else CHECK(uqa_value_get_int64(arguments[0], &scan->count));
    *out_scan = scan;
    ++data->created;
    return UQA_OK;
}

static uqa_status UQA_CALL next_row(uqa_udf_row_sink sink, void *state, void *user_data) {
    struct data *data = (struct data *)user_data;
    struct scan *scan = (struct scan *)state;
    ++data->next;
    if (data->mode == NEXT_FAIL && scan->current == 1) return uqa_callback_fail(UQA_ERROR_IO, "scan next failed");
    if (data->mode == NO_ROW) return UQA_OK;
    if (scan->current == scan->count) return UQA_DONE;
    if (data->mode == CANCEL) CHECK(uqa_cancel_handle_request(data->cancellation));
    uqa_value value = NULL;
    ++scan->current;
    if (data->mode == ENUM_ARGUMENT) CHECK(uqa_value_create_text(data->label, &value));
    else CHECK(uqa_value_create_int64(scan->current, &value));
    uqa_value_view row[] = {value};
    if (data->mode == RETRY_EMIT) assert(uqa_udf_row_sink_emit(sink, NULL, 0) == UQA_ERROR_INVALID_ARGUMENT);
    uqa_status status = uqa_udf_row_sink_emit(sink, row, 1);
    if (data->mode == RETRY_EMIT) assert(uqa_udf_row_sink_emit(sink, row, 1) == UQA_ERROR_STATE);
    CHECK(uqa_value_destroy(&value));
    if (status != UQA_OK) return status;
    return data->mode == DONE_ROW ? UQA_DONE : UQA_OK;
}

static uqa_status UQA_CALL destroy_scan(void *state, void *user_data) {
    struct data *data = (struct data *)user_data;
    ++data->destroyed;
    assert(data->destroyed <= data->created);
    free(state);
    return data->fail_destroy ? uqa_callback_fail(UQA_ERROR_IO, "scan cleanup failed") : UQA_OK;
}

static uqa_status UQA_CALL destroy_data(void *user_data) {
    struct data *data = (struct data *)user_data;
    assert(data->created == data->destroyed);
    assert(++data->released == 1);
    return UQA_OK;
}

int main(void) {
    struct data data;
    memset(&data, 0, sizeof(data));
    uqa_engine engine = NULL;
    uqa_result result = NULL;
    uqa_udf function = NULL;
    uqa_udf_options options = NULL;
    CHECK(test_database_open(NULL, &engine));
    CHECK(uqa_result_create(&result));
    CHECK(uqa_cancel_handle_create(engine, &data.cancellation));
    CHECK(uqa_udf_options_create(&options));
    CHECK(uqa_udf_options_set_may_mutate_engine(options, 0));
    const char *columns[] = {"n"};
    assert(uqa_udf_register_table(engine, "c_rows", options, columns, 1, create_scan, NULL, destroy_scan, &data, destroy_data, &function) == UQA_ERROR_INVALID_ARGUMENT);
    assert(function == NULL && data.released == 0);
    assert(uqa_udf_register_table(engine, "c_rows", options, NULL, 1, create_scan, next_row, destroy_scan, &data, destroy_data, &function) == UQA_ERROR_INVALID_ARGUMENT);
    assert(uqa_udf_register_table(engine, "c_rows", options, columns, 0, create_scan, next_row, destroy_scan, &data, destroy_data, &function) == UQA_ERROR_INVALID_ARGUMENT);
    assert(function == NULL && data.released == 0 && data.created == 0);
    CHECK(uqa_udf_register_table(engine, "c_rows", options, columns, 1, create_scan, next_row, destroy_scan, &data, destroy_data, &function));
    columns[0] = "changed_after_registration";
    CHECK(uqa_udf_options_destroy(&options));
    data.mode = ENUM_ARGUMENT;
    data.label = "ready";
    CHECK(uqa_engine_execute(engine, "CREATE TYPE host_mood AS ENUM ('ready'); CREATE TABLE host_values (m host_mood); INSERT INTO host_values VALUES ('ready')", result));
    CHECK(uqa_engine_execute(engine, "SELECT r.n FROM host_values, LATERAL c_rows(m) AS r(n)", result));
    uqa_value_view label_value = NULL;
    uqa_string_view label = {NULL, 0};
    CHECK(uqa_result_get_value(result, 0, 0, &label_value));
    CHECK(uqa_value_get_text(label_value, &label));
    assert(udf_equal(label, "ready"));
    CHECK(uqa_engine_execute(engine, "SELECT n FROM host_values, LATERAL c_rows(m)", result));
    CHECK(uqa_result_get_value(result, 0, 0, &label_value));
    CHECK(uqa_value_get_text(label_value, &label));
    assert(udf_equal(label, "ready"));
    CHECK(uqa_engine_execute(engine, "SELECT c_rows(m) AS echoed FROM host_values", result));
    CHECK(uqa_result_get_value(result, 0, 0, &label_value));
    CHECK(uqa_value_get_text(label_value, &label));
    assert(udf_equal(label, "ready"));
    CHECK(uqa_engine_execute(engine, "ALTER TYPE host_mood RENAME VALUE 'ready' TO 'renamed'", result));
    data.label = "renamed";
    CHECK(uqa_engine_execute(engine, "SELECT r.n FROM host_values, LATERAL c_rows(m) AS r(n)", result));
    CHECK(uqa_result_get_value(result, 0, 0, &label_value));
    CHECK(uqa_value_get_text(label_value, &label));
    assert(udf_equal(label, "renamed"));
    data.mode = ROWS;
    data.mode = CREATE_FAIL;
    int untouched = data.created;
    const char *empty_queries[] = {
        "SELECT n FROM c_rows(1) LIMIT 0",
        "SELECT n FROM (SELECT 0 AS k WHERE FALSE) v, LATERAL c_rows(k)",
        "SELECT n FROM (VALUES(1)) v(k), LATERAL c_rows(k) LIMIT 0"
    };
    for (size_t i = 0; i < sizeof(empty_queries) / sizeof(empty_queries[0]); ++i) {
        CHECK(uqa_engine_execute(engine, empty_queries[i], result));
        uint64_t rows = 1;
        uint32_t column_count = 0;
        CHECK(uqa_result_get_row_count(result, &rows));
        CHECK(uqa_result_get_column_count(result, &column_count));
        assert(rows == 0 && column_count == 1 && data.created == untouched);
    }
    data.mode = ROWS;
    CHECK(uqa_engine_execute(engine, "SELECT sum(n) FROM c_rows(4)", result));
    assert(udf_result_int(result, 0, 0) == 10 && data.created == data.destroyed);
    CHECK(uqa_engine_execute(engine, "SELECT c_rows(3)", result));
    assert(udf_result_int(result, 2, 0) == 3 && data.created == data.destroyed);
    data.next = 0;
    CHECK(uqa_engine_execute(engine, "SELECT n FROM c_rows(1000000) LIMIT 1", result));
    assert(udf_result_int(result, 0, 0) == 1);
    assert(data.next <= 1024 && data.created == data.destroyed);
    CHECK(uqa_engine_execute(engine, "WITH x AS MATERIALIZED (SELECT n FROM c_rows(1000000)) SELECT n FROM x LIMIT 1", result));
    assert(udf_result_int(result, 0, 0) == 1 && data.created == data.destroyed);
    CHECK(uqa_engine_execute(engine, "SELECT sum(r.n) FROM (VALUES (1), (3)) AS v(k), LATERAL c_rows(v.k) AS r(n)", result));
    assert(udf_result_int(result, 0, 0) == 7 && data.created == data.destroyed);
    data.mode = RETRY_EMIT;
    CHECK(uqa_engine_execute(engine, "SELECT sum(n) FROM c_rows(3)", result));
    assert(udf_result_int(result, 0, 0) == 6 && data.created == data.destroyed);

    data.mode = CREATE_FAIL;
    int before = data.created;
    assert(uqa_engine_execute(engine, "SELECT * FROM c_rows(3)", result) == UQA_ERROR_SQL);
    udf_failure("scan creation failed", UQA_ERROR_IO);
    assert(data.created == before && data.destroyed == before);
    data.mode = NO_ROW;
    assert(uqa_engine_execute(engine, "SELECT * FROM c_rows(3)", result) == UQA_ERROR_SQL);
    udf_failure("table callback returned UQA_OK without a row", UQA_ERROR_STATE);
    assert(data.created == data.destroyed);
    data.mode = DONE_ROW;
    assert(uqa_engine_execute(engine, "SELECT * FROM c_rows(3)", result) == UQA_ERROR_SQL);
    udf_failure("table callback emitted a row with UQA_DONE", UQA_ERROR_STATE);
    assert(data.created == data.destroyed);
    data.mode = NEXT_FAIL;
    data.fail_destroy = 1;
    assert(uqa_engine_execute(engine, "SELECT * FROM c_rows(3)", result) == UQA_ERROR_SQL);
    udf_failure("scan next failed", UQA_ERROR_IO);
    uqa_error error = NULL;
    uqa_string_view detail = {NULL, 0};
    CHECK(uqa_error_get_last(&error));
    CHECK(uqa_error_get_detail(error, &detail));
    assert(detail.size > 0);
    assert(data.created == data.destroyed);
    data.mode = ROWS;
    assert(uqa_engine_execute(engine, "SELECT * FROM c_rows(3) LIMIT 1", result) == UQA_ERROR_SQL);
    udf_failure("scan cleanup failed", UQA_ERROR_IO);
    assert(data.created == data.destroyed);
    data.fail_destroy = 0;

    CHECK(uqa_engine_execute(engine, "BEGIN; SAVEPOINT before_callback", result));
    data.mode = NEXT_FAIL;
    assert(uqa_engine_execute(engine, "SELECT * FROM c_rows(3)", result) == UQA_ERROR_SQL);
    uint32_t transaction = 0;
    CHECK(uqa_transaction_get_state(engine, &transaction));
    assert(transaction == UQA_TRANSACTION_FAILED);
    CHECK(uqa_savepoint_rollback(engine, "before_callback"));
    data.mode = ROWS;
    CHECK(uqa_engine_execute(engine, "SELECT sum(n) FROM c_rows(2)", result));
    assert(udf_result_int(result, 0, 0) == 3);
    CHECK(uqa_transaction_rollback(engine));
    data.mode = CANCEL;
    assert(uqa_engine_execute(engine, "SELECT * FROM c_rows(3)", result) == UQA_ERROR_SQL);
    udf_sqlstate("57014");
    assert(data.created == data.destroyed);
    CHECK(uqa_engine_reset_cancellation(engine));
    data.mode = ROWS;

    /* A native SQL portal owns its scan until close, independently of C results. */
    CHECK(uqa_engine_execute(engine, "BEGIN; DECLARE c_portal CURSOR FOR SELECT n FROM c_rows(1000000)", result));
    CHECK(uqa_engine_execute(engine, "FETCH 1 FROM c_portal", result));
    assert(udf_result_int(result, 0, 0) == 1);
    uqa_udf original = function;
    assert(uqa_udf_unregister(&function) == UQA_ERROR_BUSY && function == original);
    CHECK(uqa_engine_execute(engine, "CLOSE c_portal; COMMIT", result));
    assert(data.created == data.destroyed);
    CHECK(uqa_udf_unregister(&function));
    assert(data.released == 1 && function == NULL);
    CHECK(uqa_cancel_handle_destroy(&data.cancellation));
    CHECK(uqa_result_destroy(&result));
    CHECK(uqa_engine_close(&engine));
    return 0;
}
