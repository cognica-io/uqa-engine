/*
 * Unified Query Algebra
 *
 * Copyright (c) 2023-2026 Cognica, Inc.
 */

#include "test_udf.h"

struct invalid_text {
    const char *data;
    size_t size;
    uqa_status status;
};

static const char invalid_utf8[] = "\xff";
static const char embedded_nul[] = {'x', '\0', 'y'};
static const struct invalid_text invalid_inputs[] = {
    {NULL, 0, UQA_ERROR_INVALID_ARGUMENT},
    {NULL, 1, UQA_ERROR_INVALID_ARGUMENT},
    {invalid_utf8, sizeof(invalid_utf8) - 1, UQA_ERROR_INVALID_ARGUMENT},
    {embedded_nul, sizeof(embedded_nul), UQA_ERROR_INVALID_ARGUMENT},
    {"x", SIZE_MAX, UQA_ERROR_RANGE}
};

struct callback_data {
    int created, destroyed, released, fail, fail_cleanup;
};

static void transaction_state(uqa_engine engine, uint32_t expected) {
    uint32_t state = UINT32_MAX;
    CHECK(uqa_transaction_get_state(engine, &state));
    assert(state == expected);
}

static uqa_status UQA_CALL destroy_data(void *pointer) {
    struct callback_data *data = (struct callback_data *)pointer;
    assert(data->created == data->destroyed);
    assert(++data->released == 1);
    return UQA_OK;
}

static uqa_status fail_context(uqa_udf_context context, int mode) {
    uqa_status status;
    if (mode == 1) {
        char message[] = {'s', 'p', 'a', 'n', ' ', 'f', 'a', 'i', 'l', 'u', 'r', 'e', '!'};
        status = uqa_udf_context_fail_n(context, message, sizeof(message) - 1);
        memset(message, '?', sizeof(message));
    } else {
        const struct invalid_text *input = &invalid_inputs[mode - 2];
        status = uqa_udf_context_fail_n(context, input->data, input->size);
        assert(status == input->status);
    }
    /* Successful cleanup must preserve the diagnostic for the returned status. */
    uqa_value temporary = NULL;
    CHECK(uqa_value_create_int64(1, &temporary));
    CHECK(uqa_value_destroy(&temporary));
    return status;
}

static uqa_status UQA_CALL scalar(uqa_udf_context context,
    const uqa_value_view *arguments, uint32_t count, void *pointer) {
    const struct callback_data *data = (const struct callback_data *)pointer;
    assert(count == 1);
    if (data->fail) return fail_context(context, data->fail);
    return uqa_udf_context_set_result(context, arguments[0]);
}

static uqa_status UQA_CALL create_state(void *pointer, void **out) {
    struct callback_data *data = (struct callback_data *)pointer;
    int64_t *state = (int64_t *)calloc(1, sizeof(*state));
    assert(state != NULL);
    *out = state;
    ++data->created;
    return UQA_OK;
}

static uqa_status UQA_CALL create_scan(const uqa_value_view *arguments,
    uint32_t count, void *pointer, void **out) {
    (void)arguments;
    assert(count == 0);
    return create_state(pointer, out);
}

static uqa_status UQA_CALL next_row(uqa_udf_row_sink sink, void *pointer, void *data) {
    int64_t *state = (int64_t *)pointer;
    (void)data;
    if ((*state)++ != 0) return UQA_DONE;
    uqa_value value = NULL;
    CHECK(uqa_value_create_int64(42, &value));
    const uqa_value_view row[] = {value};
    uqa_status status = uqa_udf_row_sink_emit(sink, row, 1);
    CHECK(uqa_value_destroy(&value));
    return status;
}

static uqa_status UQA_CALL observe(void *pointer,
    const uqa_value_view *arguments, uint32_t count, void *data) {
    (void)data;
    assert(count == 1);
    int64_t value = 0;
    CHECK(uqa_value_get_int64(arguments[0], &value));
    *(int64_t *)pointer += value;
    return UQA_OK;
}

static uqa_status UQA_CALL finish(uqa_udf_context context, void *pointer, void *data) {
    const struct callback_data *callback = (const struct callback_data *)data;
    if (callback->fail) return fail_context(context, callback->fail);
    return udf_set_int(context, *(const int64_t *)pointer);
}

static uqa_status UQA_CALL destroy_state(void *pointer, void *user_data) {
    struct callback_data *data = (struct callback_data *)user_data;
    free(pointer);
    ++data->destroyed;
    assert(data->destroyed <= data->created);
    return data->fail_cleanup ? uqa_callback_fail(UQA_ERROR_IO, "cleanup failed") : UQA_OK;
}

static void open_database(const char *path, uqa_engine *engine) {
    const uqa_open_flags flags = test_database_flags();
    if (flags == UQA_OPEN_MEMORY) {
        CHECK(uqa_engine_open_n(NULL, 0, flags, NULL, engine));
    } else {
        const size_t size = strlen(path);
        char *bytes = (char *)malloc(size);
        assert(bytes != NULL);
        memcpy(bytes, path, size);
        CHECK(uqa_engine_open_n(bytes, size, flags, NULL, engine));
        memset(bytes, '?', size);
        free(bytes);
    }
}

static void invalid_open(void) {
    uqa_engine engine = NULL;
    assert(uqa_engine_open_n(NULL, 1, UQA_OPEN_MEMORY, NULL, &engine) == UQA_ERROR_INVALID_ARGUMENT);
    assert(uqa_engine_open_n("", 0, UQA_OPEN_MEMORY, NULL, &engine) == UQA_ERROR_INVALID_ARGUMENT);
    assert(uqa_engine_open_n(NULL, 0, 0, NULL, &engine) == UQA_ERROR_INVALID_ARGUMENT);
    assert(engine == NULL);
    if (test_database_flags() == UQA_OPEN_MEMORY) return;
    for (size_t i = 0; i < sizeof(invalid_inputs) / sizeof(invalid_inputs[0]); ++i) {
        const struct invalid_text *input = &invalid_inputs[i];
        assert(uqa_engine_open_n(input->data, input->size, test_database_flags(), NULL, &engine) == input->status);
        assert(engine == NULL);
    }
    assert(uqa_engine_open_n("", 0, test_database_flags(), NULL, &engine) == UQA_ERROR_INVALID_ARGUMENT);
    assert(engine == NULL);
}

static void user_session(uqa_engine engine, uqa_result result) {
    uqa_engine sibling = NULL;
    CHECK(uqa_engine_execute(engine, "CREATE ROLE span_user LOGIN", result));
    for (size_t i = 0; i < sizeof(invalid_inputs) / sizeof(invalid_inputs[0]); ++i) {
        const struct invalid_text *input = &invalid_inputs[i];
        assert(uqa_engine_open_session_for_user_n(engine, input->data, input->size, &sibling) == input->status);
        assert(sibling == NULL);
    }
    char role[] = {'s', 'p', 'a', 'n', '_', 'u', 's', 'e', 'r', '!'};
    uqa_status status = uqa_engine_open_session_for_user_n(engine, role, sizeof(role) - 1, &sibling);
    memset(role, '?', sizeof(role));
    if (test_database_flags() == UQA_OPEN_MEMORY) {
        assert(status == UQA_ERROR_UNSUPPORTED && sibling == NULL);
    } else {
        CHECK(status);
        uqa_result session_result = NULL;
        CHECK(uqa_result_create(&session_result));
        CHECK(uqa_engine_execute(sibling, "SELECT current_user", session_result));
        uqa_value_view value = NULL;
        uqa_string_view text = {NULL, 0};
        CHECK(uqa_result_get_value(session_result, 0, 0, &value));
        CHECK(uqa_value_get_text(value, &text));
        assert(udf_equal(text, "span_user"));
        CHECK(uqa_result_destroy(&session_result));
        CHECK(uqa_engine_close(&sibling));
    }
}

static void savepoints(uqa_engine engine, uqa_result result) {
    CHECK(uqa_engine_execute(engine, "CREATE TABLE span_values (n integer)", result));
    CHECK(uqa_transaction_begin(engine));
    char name[] = {'b', 'e', 'f', 'o', 'r', 'e', ' ', '"', '\xc3', '\xa9'};
    CHECK(uqa_savepoint_create_n(engine, name, sizeof(name)));
    CHECK(uqa_engine_execute(engine, "INSERT INTO span_values VALUES (1)", result));
    for (size_t i = 0; i < sizeof(invalid_inputs) / sizeof(invalid_inputs[0]); ++i) {
        const struct invalid_text *input = &invalid_inputs[i];
        assert(uqa_savepoint_create_n(engine, input->data, input->size) == input->status);
        assert(uqa_savepoint_rollback_n(engine, input->data, input->size) == input->status);
        assert(uqa_savepoint_release_n(engine, input->data, input->size) == input->status);
        transaction_state(engine, UQA_TRANSACTION_ACTIVE);
    }
    assert(uqa_engine_execute(engine, "SELECT 1 / 0", result) == UQA_ERROR_SQL);
    transaction_state(engine, UQA_TRANSACTION_FAILED);
    CHECK(uqa_savepoint_rollback_n(engine, name, sizeof(name)));
    transaction_state(engine, UQA_TRANSACTION_ACTIVE);
    CHECK(uqa_engine_execute(engine, "SELECT count(*) FROM span_values", result));
    assert(udf_result_int(result, 0, 0) == 0);
    CHECK(uqa_savepoint_release_n(engine, name, sizeof(name)));
    memset(name, '?', sizeof(name));
    CHECK(uqa_engine_execute(engine, "INSERT INTO span_values VALUES (7)", result));
    CHECK(uqa_transaction_commit(engine));
}

static void registrations(uqa_engine engine, uqa_result result) {
    struct callback_data data[3];
    memset(data, 0, sizeof(data));
    uqa_udf scalar_handle = NULL, table_handle = NULL, aggregate_handle = NULL;
    uqa_udf_options options = NULL;
    CHECK(uqa_udf_options_create(&options));
    CHECK(uqa_udf_options_set_may_mutate_engine(options, 0));
    char scalar_name[] = "span_scalar!", table_name[] = "span_rows!", aggregate_name[] = "span_total!";
    char column[] = {'v', 'a', 'l', 'u', 'e', '!', '!'};
    const char *columns[] = {column};
    size_t sizes[] = {5};
    for (size_t i = 0; i < sizeof(invalid_inputs) / sizeof(invalid_inputs[0]); ++i) {
        const struct invalid_text *input = &invalid_inputs[i];
        assert(uqa_udf_register_scalar_n(engine, input->data, input->size, options, scalar,
            &data[0], destroy_data, &scalar_handle) == input->status);
        assert(uqa_udf_register_table_n(engine, input->data, input->size, options, columns, sizes, 1,
            create_scan, next_row, destroy_state, &data[1], destroy_data, &table_handle) == input->status);
        assert(uqa_udf_register_aggregate_n(engine, input->data, input->size, options,
            create_state, observe, finish, destroy_state, &data[2], destroy_data, &aggregate_handle) == input->status);
        const char *bad_columns[] = {input->data};
        assert(uqa_udf_register_table_n(engine, table_name, sizeof(table_name) - 2, options,
            bad_columns, &input->size, 1, create_scan, next_row, destroy_state,
            &data[1], destroy_data, &table_handle) == input->status);
        assert(scalar_handle == NULL && table_handle == NULL && aggregate_handle == NULL);
        assert(data[0].released == 0 && data[1].released == 0 && data[2].released == 0);
    }
    assert(uqa_udf_register_table_n(engine, table_name, sizeof(table_name) - 2, options,
        columns, NULL, 1, create_scan, next_row, destroy_state,
        &data[1], destroy_data, &table_handle) == UQA_ERROR_INVALID_ARGUMENT);
    assert(uqa_udf_register_table_n(engine, table_name, sizeof(table_name) - 2, options,
        NULL, sizes, 1, create_scan, next_row, destroy_state,
        &data[1], destroy_data, &table_handle) == UQA_ERROR_INVALID_ARGUMENT);
    assert(uqa_udf_register_table_n(engine, table_name, sizeof(table_name) - 2, options,
        NULL, NULL, 0, create_scan, next_row, destroy_state,
        &data[1], destroy_data, &table_handle) == UQA_ERROR_INVALID_ARGUMENT);
    assert(table_handle == NULL && data[1].created == 0 && data[1].released == 0);
    CHECK(uqa_udf_register_scalar_n(engine, scalar_name, sizeof(scalar_name) - 2, options,
        scalar, &data[0], destroy_data, &scalar_handle));
    CHECK(uqa_udf_register_table_n(engine, table_name, sizeof(table_name) - 2, options,
        columns, sizes, 1, create_scan, next_row, destroy_state,
        &data[1], destroy_data, &table_handle));
    CHECK(uqa_udf_register_aggregate_n(engine, aggregate_name, sizeof(aggregate_name) - 2, options,
        create_state, observe, finish, destroy_state, &data[2], destroy_data, &aggregate_handle));
    memset(scalar_name, '?', sizeof(scalar_name));
    memset(table_name, '?', sizeof(table_name));
    memset(aggregate_name, '?', sizeof(aggregate_name));
    memset(column, '?', sizeof(column));
    columns[0] = NULL;
    sizes[0] = SIZE_MAX;
    CHECK(uqa_udf_options_destroy(&options));
    CHECK(uqa_engine_execute(engine, "SELECT span_scalar(17), value FROM span_rows()", result));
    assert(udf_result_int(result, 0, 0) == 17 && udf_result_int(result, 0, 1) == 42);
    CHECK(uqa_engine_execute(engine, "SELECT span_total(n) FROM (VALUES (2), (3)) AS v(n)", result));
    assert(udf_result_int(result, 0, 0) == 5);
    data[0].fail = 1;
    assert(uqa_engine_execute(engine, "SELECT span_scalar(1)", result) == UQA_ERROR_SQL);
    udf_failure("span failure", UQA_ERROR_SQL);
    for (size_t i = 0; i < sizeof(invalid_inputs) / sizeof(invalid_inputs[0]); ++i) {
        data[0].fail = (int)i + 2;
        assert(uqa_engine_execute(engine, "SELECT span_scalar(1)", result) == UQA_ERROR_SQL);
        uqa_error error = NULL, cause = NULL;
        uqa_status status = UQA_OK;
        CHECK(uqa_error_get_last(&error));
        CHECK(uqa_error_get_cause(error, &cause));
        CHECK(uqa_error_get_status(cause, &status));
        assert(status == invalid_inputs[i].status);
    }
    data[0].fail = 0;
    CHECK(uqa_engine_execute(engine, "SELECT span_scalar(19)", result));
    assert(udf_result_int(result, 0, 0) == 19);
    data[2].fail = 1;
    data[2].fail_cleanup = 1;
    assert(uqa_engine_execute(engine, "SELECT span_total(n) FROM (VALUES (2), (3)) AS v(n)", result) == UQA_ERROR_SQL);
    udf_failure("span failure", UQA_ERROR_SQL);
    assert(data[2].created == data[2].destroyed);
    CHECK(uqa_udf_unregister(&scalar_handle));
    CHECK(uqa_udf_unregister(&table_handle));
    CHECK(uqa_udf_unregister(&aggregate_handle));
    assert(data[0].released == 1 && data[1].released == 1 && data[2].released == 1);
}

int main(void) {
    const char *directory = getenv("UQA_TEST_DATABASE_DIR");
    assert(directory != NULL);
    char path[4096];
    int size = snprintf(path, sizeof(path), "%s/string-spans", directory);
    assert(size > 0 && (size_t)size < sizeof(path));
    invalid_open();
    uqa_engine engine = NULL;
    uqa_result result = NULL;
    open_database(path, &engine);
    CHECK(uqa_result_create(&result));
    user_session(engine, result);
    savepoints(engine, result);
    registrations(engine, result);
    CHECK(uqa_result_destroy(&result));
    CHECK(uqa_engine_close(&engine));
    if (test_database_flags() != UQA_OPEN_MEMORY) {
        open_database(path, &engine);
        CHECK(uqa_result_create(&result));
        CHECK(uqa_engine_execute(engine, "SELECT n FROM span_values", result));
        assert(udf_result_int(result, 0, 0) == 7);
        CHECK(uqa_result_destroy(&result));
        CHECK(uqa_engine_close(&engine));
    }
    return 0;
}
