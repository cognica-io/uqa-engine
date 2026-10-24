/*
 * Unified Query Algebra
 *
 * Copyright (c) 2023-2026 Cognica, Inc.
 */

#include <uqa/uqa.h>
#include <assert.h>
#include <dirent.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>

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

static int64_t integer(uqa_result result, uint64_t row, uint32_t column) {
    uqa_value_view value = NULL;
    int64_t number = 0;
    CHECK(uqa_result_get_value(result, row, column, &value));
    CHECK(uqa_value_get_int64(value, &number));
    return number;
}

/* The harness gives each client an isolated temporary directory. */
static size_t temporary_files(const char *path) {
    size_t count = 0;
    DIR *directory = opendir(path);
    struct dirent *entry;
    assert(directory != NULL);
    while ((entry = readdir(directory)) != NULL) {
        char full[4096];
        struct stat info;
        int length;
        if (strcmp(entry->d_name, ".") == 0 || strcmp(entry->d_name, "..") == 0) continue;
        length = snprintf(full, sizeof(full), "%s/%s", path, entry->d_name);
        assert(length > 0 && (size_t)length < sizeof(full));
        assert(stat(full, &info) == 0);
        count += S_ISDIR(info.st_mode) ? temporary_files(full) : 1;
    }
    assert(closedir(directory) == 0);
    return count;
}

static void empty_descriptor(uqa_engine engine, uqa_result result) {
    uqa_cursor cursor = NULL;
    uqa_string_view name = {NULL, 0};
    uqa_type type = NULL;
    uint32_t columns = 99, kind = 99;
    uint64_t count = 99;
    CHECK(uqa_engine_execute(engine, "SELECT 99", result));
    CHECK(uqa_cursor_open(engine,
        "SELECT 1::integer AS x, 'text'::text AS x WHERE false", &cursor));
    CHECK(uqa_cursor_get_row_count(cursor, &count));
    CHECK(uqa_cursor_get_column_count(cursor, &columns));
    assert(count == 0 && columns == 2);
    CHECK(uqa_cursor_get_column_name(cursor, 0, &name));
    assert(equal(name, "x"));
    CHECK(uqa_cursor_get_column_name(cursor, 1, &name));
    assert(equal(name, "x"));
    CHECK(uqa_cursor_get_column_type(cursor, 0, &type));
    CHECK(uqa_type_get_kind(type, &kind));
    assert(kind == UQA_TYPE_INT32);
    CHECK(uqa_cursor_get_column_type(cursor, 1, &type));
    CHECK(uqa_type_get_kind(type, &kind));
    assert(kind == UQA_TYPE_TEXT);
    assert(uqa_cursor_get_column_type(cursor, 2, &type) == UQA_ERROR_RANGE);
    assert(type == NULL);
    assert(uqa_cursor_fetch(cursor, 0, result) == UQA_ERROR_INVALID_ARGUMENT);
    assert(uqa_cursor_fetch(cursor, 1, result) == UQA_DONE);
    assert(uqa_cursor_fetch(cursor, 1, result) == UQA_DONE);
    assert(integer(result, 0, 0) == 99);
    CHECK(uqa_cursor_close(&cursor));
    CHECK(uqa_cursor_close(&cursor));
}

static void retained_slices(uqa_engine engine, uqa_result result) {
    uqa_cursor cursor = NULL;
    uqa_result first = NULL;
    uqa_value_view first_value = NULL;
    uqa_string_view text = {NULL, 0};
    uint64_t count = 0, seen = 0;
    uint32_t kind = 99;
    size_t before;
    CHECK(uqa_engine_execute(engine, "SET work_mem = '64kB'", result));
    CHECK(uqa_result_create(&first));
    before = temporary_files(getenv("TMPDIR"));
    CHECK(uqa_cursor_open(engine,
        "SELECT n AS x, n + 10000 AS x, repeat('p', 128) AS payload "
        "FROM generate_series(1, 2061) AS s(n) ORDER BY n", &cursor));
    assert(temporary_files(getenv("TMPDIR")) > before);
    CHECK(uqa_cursor_get_row_count(cursor, &count));
    assert(count == 2061);
    assert(uqa_engine_close(&engine) == UQA_ERROR_BUSY);
    assert(uqa_cursor_open(engine, "SELECT 0", &cursor) == UQA_ERROR_INVALID_ARGUMENT);
    CHECK(uqa_cursor_fetch(cursor, 1, first));
    CHECK(uqa_result_get_value(first, 0, 2, &first_value));
    CHECK(uqa_value_get_text(first_value, &text));
    assert(text.size == 128 && text.data[0] == 'p');
    CHECK(uqa_result_get_kind(first, &kind));
    assert(kind == UQA_RESULT_ROWS);
    CHECK(uqa_result_get_affected_rows(first, &count));
    assert(count == 0);
    assert(uqa_result_get_command_tag(first, &text) == UQA_DONE);
    assert(text.data == NULL && text.size == 0);
    seen = 1;
    for (;;) {
        uqa_status status = uqa_cursor_fetch(cursor, 17, result);
        if (status == UQA_DONE) break;
        CHECK(status);
        CHECK(uqa_result_get_row_count(result, &count));
        assert(count > 0 && count <= 17);
        for (uint64_t row = 0; row < count; ++row) {
            assert(integer(result, row, 0) == (int64_t)(seen + row + 1));
            assert(integer(result, row, 1) == (int64_t)(seen + row + 10001));
        }
        seen += count;
    }
    assert(seen == 2061);
    assert(temporary_files(getenv("TMPDIR")) == before);
    CHECK(uqa_cursor_get_column_name(cursor, 1, &text));
    assert(equal(text, "x"));
    CHECK(uqa_cursor_close(&cursor));
    assert(integer(first, 0, 0) == 1 && integer(first, 0, 1) == 10001);
    CHECK(uqa_value_get_text(first_value, &text));
    assert(text.size == 128 && text.data[127] == 'p');
    CHECK(uqa_result_get_column_name(first, 1, &text));
    assert(equal(text, "x"));
    CHECK(uqa_result_destroy(&first));
}

static void parameters_and_admission(uqa_engine engine, uqa_result result) {
    uqa_cursor cursor = NULL;
    uqa_value input = NULL;
    uqa_value_view params[1];
    const char query[] = {'S', 'E', 'L', 'E', 'C', 'T', ' ', '1', '7'};
    CHECK(uqa_cursor_open_n(engine, query, sizeof(query), &cursor));
    assert(uqa_cursor_fetch(cursor, 1, NULL) == UQA_ERROR_INVALID_ARGUMENT);
    CHECK(uqa_cursor_fetch(cursor, 1, result));
    assert(integer(result, 0, 0) == 17);
    CHECK(uqa_cursor_close(&cursor));
    CHECK(uqa_value_create_int64(41, &input));
    params[0] = input;
    CHECK(uqa_cursor_open_params(engine, "SELECT $1 + 1", params, 1, &cursor));
    CHECK(uqa_value_destroy(&input));
    CHECK(uqa_cursor_fetch(cursor, 1, result));
    assert(integer(result, 0, 0) == 42);
    CHECK(uqa_cursor_close(&cursor));
    CHECK(uqa_result_get_value(result, 0, 0, &params[0]));
    CHECK(uqa_cursor_open_params_n(engine, "SELECT $1 + 1!", 13, params, 1, &cursor));
    CHECK(uqa_cursor_fetch(cursor, 1, result));
    assert(integer(result, 0, 0) == 43);
    CHECK(uqa_cursor_close(&cursor));
    assert(uqa_cursor_open(engine, "SELECT 1; SELECT 2", &cursor) == UQA_ERROR_SQL);
    assert(cursor == NULL && integer(result, 0, 0) == 43);
    assert(uqa_cursor_open(engine, "CREATE TABLE denied(n integer)", &cursor) == UQA_ERROR_SQL);
    assert(cursor == NULL);
    assert(uqa_engine_execute(engine, "SELECT * FROM denied", result) == UQA_ERROR_SQL);
    assert(uqa_cursor_open_n(engine, "SELECT\0 1", 9, &cursor) == UQA_ERROR_INVALID_ARGUMENT);
    assert(cursor == NULL);
}

static void terminal_cancellation(uqa_engine engine, uqa_result result, int explicit_tx) {
    uqa_cursor cursor = NULL;
    uqa_cancel_handle cancel = NULL;
    uqa_error original = NULL, repeated = NULL;
    uqa_string_view state = {NULL, 0}, message = {NULL, 0};
    char saved[512];
    uint32_t completion = 99, tx_state = 99;
    size_t before = temporary_files(getenv("TMPDIR"));
    if (explicit_tx) CHECK(uqa_transaction_begin(engine));
    CHECK(uqa_cursor_open(engine,
        "SELECT n, repeat('c', 128) FROM generate_series(1, 2061) AS s(n)", &cursor));
    assert(temporary_files(getenv("TMPDIR")) > before);
    CHECK(uqa_cursor_fetch(cursor, 1, result));
    CHECK(uqa_cancel_handle_create(engine, &cancel));
    CHECK(uqa_cancel_handle_request(cancel));
    assert(uqa_cursor_fetch(cursor, 1, result) == UQA_ERROR_SQL);
    assert(temporary_files(getenv("TMPDIR")) == before);
    CHECK(uqa_error_get_last(&original));
    CHECK(uqa_error_get_sqlstate(original, &state));
    assert(equal(state, "57014"));
    CHECK(uqa_error_get_completion(original, &completion));
    assert(completion == (explicit_tx ? UQA_COMPLETION_IN_TRANSACTION : UQA_COMPLETION_COMMITTED));
    CHECK(uqa_error_get_message(original, &message));
    assert(message.size < sizeof(saved));
    memcpy(saved, message.data, message.size);
    saved[message.size] = '\0';
    CHECK(uqa_engine_reset_cancellation(engine));
    assert(integer(result, 0, 0) == 1);
    CHECK(uqa_transaction_get_state(engine, &tx_state));
    assert(tx_state == (explicit_tx ? UQA_TRANSACTION_ACTIVE : UQA_TRANSACTION_IDLE));
    CHECK(uqa_engine_execute(engine, "SELECT 77", result));
    assert(uqa_cursor_fetch(cursor, 1, result) == UQA_ERROR_SQL);
    CHECK(uqa_error_get_last(&repeated));
    assert(original == repeated);
    CHECK(uqa_error_get_message(repeated, &message));
    assert(equal(message, saved));
    assert(integer(result, 0, 0) == 77);
    CHECK(uqa_cursor_close(&cursor));
    CHECK(uqa_cancel_handle_destroy(&cancel));
    if (explicit_tx) CHECK(uqa_transaction_rollback(engine));
}

static void early_close(uqa_engine engine, uqa_result result) {
    uqa_cursor cursor = NULL;
    uqa_string_view text = {NULL, 0};
    uqa_value_view value = NULL;
    size_t before = temporary_files(getenv("TMPDIR"));
    CHECK(uqa_cursor_open(engine,
        "SELECT n, repeat('e', 128) FROM generate_series(1, 2061) AS s(n)", &cursor));
    assert(temporary_files(getenv("TMPDIR")) > before);
    CHECK(uqa_cursor_fetch(cursor, 1, result));
    CHECK(uqa_result_get_value(result, 0, 1, &value));
    CHECK(uqa_cursor_close(&cursor));
    assert(temporary_files(getenv("TMPDIR")) == before);
    assert(integer(result, 0, 0) == 1);
    CHECK(uqa_value_get_text(value, &text));
    assert(text.size == 128 && text.data[0] == 'e');
}

static void retention_limit(void) {
    uqa_config config = NULL;
    uqa_engine engine = NULL;
    uqa_cursor cursor = NULL;
    uqa_result retained[64] = {NULL};
    uqa_result previous = NULL;
    uqa_error error = NULL;
    uint32_t completion = 99;
    uint64_t count = 99;
    size_t filled = 0;
    CHECK(uqa_config_create(&config));
    CHECK(uqa_config_set_ffi_memory_limit(config, 4096));
    CHECK(uqa_engine_open_memory(config, &engine));
    CHECK(uqa_config_destroy(&config));
    CHECK(uqa_result_create(&previous));
    CHECK(uqa_engine_execute(engine, "SELECT 999", previous));
    CHECK(uqa_cursor_open(engine, "SELECT n FROM generate_series(1, 1000) AS s(n)", &cursor));
    for (; filled < 64; ++filled) {
        CHECK(uqa_result_create(&retained[filled]));
        uqa_status status = uqa_cursor_fetch(cursor, 1, retained[filled]);
        if (status == UQA_ERROR_LIMIT) break;
        CHECK(status);
    }
    assert(filled > 0 && filled < 64);
    CHECK(uqa_error_get_last(&error));
    CHECK(uqa_error_get_completion(error, &completion));
    assert(completion == UQA_COMPLETION_COMMITTED);
    assert(uqa_result_get_row_count(retained[filled], &count) == UQA_ERROR_STATE);
    assert(count == 0);
    assert(uqa_cursor_fetch(cursor, 1, previous) == UQA_ERROR_LIMIT);
    for (size_t index = 0; index <= filled; ++index) {
        CHECK(uqa_result_destroy(&retained[index]));
    }
    assert(integer(previous, 0, 0) == 999);
    assert(uqa_cursor_fetch(cursor, 1, previous) == UQA_ERROR_LIMIT);
    CHECK(uqa_cursor_close(&cursor));
    CHECK(uqa_result_destroy(&previous));
    CHECK(uqa_engine_close(&engine));
}

int main(void) {
    uqa_engine engine = NULL;
    uqa_result result = NULL;
    CHECK(uqa_engine_open_memory(NULL, &engine));
    CHECK(uqa_result_create(&result));
    empty_descriptor(engine, result);
    retained_slices(engine, result);
    parameters_and_admission(engine, result);
    terminal_cancellation(engine, result, 0);
    terminal_cancellation(engine, result, 1);
    early_close(engine, result);
    CHECK(uqa_result_destroy(&result));
    CHECK(uqa_engine_close(&engine));
    retention_limit();
    return 0;
}
