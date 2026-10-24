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

static int equal(uqa_string_view value, const char *text) {
    size_t size = strlen(text);
    return value.size == size && (size == 0 || memcmp(value.data, text, size) == 0);
}

enum mode { NORMAL, FAILED_READ, FAILED_WRITE, FAILED_FLUSH, INVALID_COUNT,
            NO_PROGRESS, BARE_FAILURE, INVALID_STATUS, CANCEL_DATA, CANCEL_EOF,
            CANCEL_FLUSH, EXHAUST_DIAGNOSTIC, IGNORED_FAILURE };

struct stream {
    uqa_engine engine, other;
    uqa_result scratch;
    uqa_value_view text, interval, foreign_value;
    uqa_cancel_handle cancellation;
    const char *input;
    size_t offset, reads, writes, flushes, size, maximum;
    char output[1024];
    enum mode mode;
    int check_helpers;
    uqa_value retained[64];
    size_t retained_count;
};

static void helpers(struct stream *stream) {
    if (!stream->check_helpers) return;
    stream->check_helpers = 0;
    uqa_string_view text = {NULL, 0};
    uqa_value value = NULL, copy = NULL;
    uqa_buffer buffer = NULL;
    uqa_bytes_view bytes = {NULL, 0};
    uqa_engine opened = NULL, alias = stream->other;
    int64_t number = 0;
    assert(uqa_engine_execute(stream->engine, "SELECT 99", stream->scratch) == UQA_ERROR_BUSY);
    assert(uqa_engine_execute(stream->other, "SELECT 99", stream->scratch) == UQA_ERROR_BUSY);
    assert(uqa_engine_open_memory(NULL, &opened) == UQA_ERROR_BUSY && opened == NULL);
    assert(uqa_engine_close(&alias) == UQA_ERROR_BUSY && alias == stream->other);
    CHECK(uqa_value_create_int64(42, &value));
    CHECK(uqa_value_get_int64(value, &number));
    assert(number == 42);
    CHECK(uqa_value_destroy(&value));
    CHECK(uqa_value_get_text(stream->text, &text));
    assert(equal(text, "visible"));
    CHECK(uqa_value_create_copy(stream->text, &copy));
    CHECK(uqa_value_destroy(&copy));
    CHECK(uqa_buffer_create(&buffer));
    CHECK(uqa_value_format_sql(stream->interval, buffer));
    CHECK(uqa_buffer_get_bytes(buffer, &bytes));
    assert(bytes.size == 3 && memcmp(bytes.data, "P1D", 3) == 0);
    CHECK(uqa_buffer_destroy(&buffer));
    assert(uqa_value_get_text(stream->foreign_value, &text) == UQA_ERROR_BUSY);
    assert(text.data == NULL && text.size == 0);
    /* An invalid helper attempt can be handled without failing the enclosing operation. */
    assert(uqa_callback_fail(UQA_DONE, "not a failure") == UQA_ERROR_INVALID_ARGUMENT);
}

static uqa_status failure(void) {
    char message[] = "original callback diagnostic";
    uqa_value value = NULL;
    uqa_error error = NULL;
    uqa_string_view text = {NULL, 0};
    uqa_status status = uqa_callback_fail_n(UQA_ERROR_IO, message, sizeof(message) - 1);
    assert(status == UQA_ERROR_IO);
    memset(message, 'x', sizeof(message) - 1);
    CHECK(uqa_error_get_last(&error));
    CHECK(uqa_error_get_message(error, &text));
    assert(equal(text, "original callback diagnostic"));
    /* These ordinary calls clear LAST_ERROR, but cannot replace the invocation's first cause. */
    CHECK(uqa_value_create_null(&value));
    CHECK(uqa_value_destroy(&value));
    assert(uqa_error_get_last(&error) == UQA_DONE && error == NULL);
    assert(uqa_callback_fail(UQA_ERROR_TYPE, "later cleanup diagnostic") == UQA_ERROR_IO);
    CHECK(uqa_value_create_null(&value));
    CHECK(uqa_value_destroy(&value));
    return status;
}

static uqa_status UQA_CALL read_bytes(void *data, uint8_t *buffer, size_t capacity, size_t *count) {
    struct stream *stream = (struct stream *)data;
    ++stream->reads;
    helpers(stream);
    if (stream->mode == FAILED_READ && stream->reads == 2) return failure();
    if (stream->mode == INVALID_COUNT) { *count = capacity + 1; return UQA_OK; }
    if (stream->mode == CANCEL_DATA || stream->mode == CANCEL_EOF) {
        CHECK(uqa_cancel_handle_request(stream->cancellation));
        buffer[0] = '1';
        *count = stream->mode == CANCEL_EOF ? 0 : 1;
        return UQA_OK;
    }
    size_t remaining = strlen(stream->input) - stream->offset;
    size_t size = remaining < capacity ? remaining : capacity;
    if (size > stream->maximum) size = stream->maximum;
    memcpy(buffer, stream->input + stream->offset, size);
    stream->offset += size;
    *count = size;
    return UQA_OK;
}

static uqa_status UQA_CALL write_bytes(void *data, const uint8_t *buffer, size_t size, size_t *count) {
    struct stream *stream = (struct stream *)data;
    ++stream->writes;
    helpers(stream);
    if (stream->mode == FAILED_WRITE && stream->writes == 2) return failure();
    if (stream->mode == INVALID_COUNT) { *count = size + 1; return UQA_OK; }
    if (stream->mode == NO_PROGRESS) { *count = 0; return UQA_OK; }
    if (stream->mode == BARE_FAILURE) return UQA_ERROR_TYPE;
    if (stream->mode == IGNORED_FAILURE) { (void)failure(); *count = size; return UQA_OK; }
    if (stream->mode == INVALID_STATUS) return UQA_DONE;
    if (stream->mode == EXHAUST_DIAGNOSTIC) {
        for (; stream->retained_count < 64; ++stream->retained_count) {
            uqa_status status = uqa_value_create_copy(stream->text, &stream->retained[stream->retained_count]);
            if (status == UQA_ERROR_LIMIT) break;
            CHECK(status);
        }
        assert(stream->retained_count > 0 && stream->retained_count < 64);
        char message[4096];
        memset(message, 'x', sizeof(message));
        return uqa_callback_fail_n(UQA_ERROR_IO, message, sizeof(message));
    }
    if (stream->mode == CANCEL_DATA) CHECK(uqa_cancel_handle_request(stream->cancellation));
    size_t copied = size < stream->maximum ? size : stream->maximum;
    assert(stream->size + copied < sizeof(stream->output));
    memcpy(stream->output + stream->size, buffer, copied);
    stream->size += copied;
    stream->output[stream->size] = 0;
    *count = copied;
    return UQA_OK;
}

static uqa_status UQA_CALL flush_bytes(void *data) {
    struct stream *stream = (struct stream *)data;
    ++stream->flushes;
    if (stream->mode == FAILED_FLUSH) return failure();
    if (stream->mode == CANCEL_FLUSH) CHECK(uqa_cancel_handle_request(stream->cancellation));
    return UQA_OK;
}

static void sql_error(const char *state, uqa_status cause_status, const char *cause_message) {
    uqa_error error = NULL, cause = NULL;
    uqa_status status = 0;
    uqa_string_view text = {NULL, 0};
    CHECK(uqa_error_get_last(&error));
    CHECK(uqa_error_get_status(error, &status));
    assert(status == UQA_ERROR_SQL);
    CHECK(uqa_error_get_sqlstate(error, &text));
    assert(equal(text, state));
    if (cause_status == UQA_OK) {
        assert(uqa_error_get_cause(error, &cause) == UQA_DONE && cause == NULL);
    } else {
        CHECK(uqa_error_get_cause(error, &cause));
        CHECK(uqa_error_get_status(cause, &status));
        assert(status == cause_status);
        CHECK(uqa_error_get_message(cause, &text));
        if (cause_message != NULL) assert(equal(text, cause_message));
        assert(uqa_error_get_sqlstate(cause, &text) == UQA_DONE);
        assert(uqa_error_get_status(error, NULL) == UQA_ERROR_INVALID_ARGUMENT);
        CHECK(uqa_error_get_status(cause, &status));
    }
}

static void unchanged(uqa_engine engine, uqa_result result) {
    uqa_value_view value = NULL;
    int64_t count = -1;
    CHECK(uqa_engine_execute(engine, "SELECT count(*) FROM copy_items", result));
    CHECK(uqa_result_get_value(result, 0, 0, &value));
    CHECK(uqa_value_get_int64(value, &count));
    assert(count == 3);
}

static void reset(struct stream *stream, enum mode mode) {
    stream->mode = mode;
    stream->offset = stream->reads = stream->writes = stream->flushes = stream->size = 0;
    stream->maximum = 2;
    stream->output[0] = 0;
}

int main(void) {
    struct stream stream;
    memset(&stream, 0, sizeof(stream));
    uqa_config config = NULL;
    uqa_result held = NULL, foreign_result = NULL;
    uqa_error error = NULL;
    uint64_t rows = 99;
    CHECK(uqa_config_create(&config));
    CHECK(uqa_config_set_ffi_memory_limit(config, 4096));
    CHECK(uqa_engine_open_memory(config, &stream.engine));
    CHECK(uqa_config_destroy(&config));
    CHECK(uqa_engine_open_memory(NULL, &stream.other));
    CHECK(uqa_result_create(&stream.scratch));
    CHECK(uqa_result_create(&held));
    CHECK(uqa_result_create(&foreign_result));
    CHECK(uqa_engine_execute(stream.engine,
        "CREATE TABLE copy_items (id INT PRIMARY KEY, body TEXT); SET intervalstyle = iso_8601; "
        "SELECT 'visible'::text, INTERVAL '1 day'", held));
    CHECK(uqa_result_get_value(held, 0, 0, &stream.text));
    CHECK(uqa_result_get_value(held, 0, 1, &stream.interval));
    CHECK(uqa_engine_execute(stream.other, "SELECT 'foreign'::text", foreign_result));
    CHECK(uqa_result_get_value(foreign_result, 0, 0, &stream.foreign_value));
    CHECK(uqa_cancel_handle_create(stream.engine, &stream.cancellation));
    assert(uqa_callback_fail(UQA_ERROR_IO, "outside") == UQA_ERROR_STATE);

    reset(&stream, NORMAL);
    stream.check_helpers = 1;
    stream.input = "1,\"a,b\"\n2,\"line\nbreak\"\n3,\n";
    const char from[] = "COPY copy_items FROM STDIN WITH (FORMAT csv)ignored";
    CHECK(uqa_engine_copy_from_n(stream.engine, from, sizeof(from) - sizeof("ignored"), read_bytes, &stream, &rows));
    assert(rows == 3 && stream.reads > 10);
    assert(uqa_error_get_last(&error) == UQA_DONE && error == NULL);
    reset(&stream, NORMAL);
    stream.check_helpers = 1;
    CHECK(uqa_engine_copy_to(stream.engine,
        "COPY (SELECT * FROM copy_items ORDER BY id) TO STDOUT WITH (FORMAT csv, HEADER)",
        write_bytes, flush_bytes, &stream, &rows));
    assert(rows == 3 && stream.flushes == 1 && stream.writes > 10);
    assert(strcmp(stream.output, "id,body\n1,\"a,b\"\n2,\"line\nbreak\"\n3,\n") == 0);
    assert(uqa_error_get_last(&error) == UQA_DONE && error == NULL);

    reset(&stream, NORMAL);
    stream.input = "4\tnew\n1\tduplicate\n";
    assert(uqa_engine_copy_from(stream.engine, "COPY copy_items FROM STDIN", read_bytes, &stream, &rows) == UQA_ERROR_SQL);
    assert(rows == 0);
    sql_error("23505", UQA_OK, NULL);
    unchanged(stream.engine, stream.scratch);
    stream.input = "4\tnew\n";
    reset(&stream, FAILED_READ);
    assert(uqa_engine_copy_from(stream.engine, "COPY copy_items FROM STDIN", read_bytes, &stream, &rows) == UQA_ERROR_SQL);
    assert(rows == 0 && stream.reads == 2);
    sql_error("58030", UQA_ERROR_IO, "original callback diagnostic");
    unchanged(stream.engine, stream.scratch);
    reset(&stream, INVALID_COUNT);
    assert(uqa_engine_copy_from(stream.engine, "COPY copy_items FROM STDIN", read_bytes, &stream, &rows) == UQA_ERROR_SQL);
    assert(rows == 0 && stream.reads == 1);
    sql_error("58030", UQA_ERROR_INVALID_ARGUMENT, NULL);
    reset(&stream, NORMAL);
    stream.input = "";
    CHECK(uqa_engine_copy_from(stream.engine, "COPY copy_items FROM STDIN", read_bytes, &stream, &rows));
    assert(rows == 0 && stream.reads == 1);

    const enum mode failures[] = {FAILED_WRITE, FAILED_FLUSH, INVALID_COUNT, NO_PROGRESS,
                                 BARE_FAILURE, INVALID_STATUS, CANCEL_DATA, CANCEL_FLUSH, IGNORED_FAILURE};
    for (size_t index = 0; index < sizeof(failures) / sizeof(failures[0]); ++index) {
        enum mode mode = failures[index];
        reset(&stream, mode);
        const char to[] = "COPY (INSERT INTO copy_items VALUES (4, 'new') RETURNING *) TO STDOUTignored";
        assert(uqa_engine_copy_to_n(stream.engine, to, sizeof(to) - sizeof("ignored"), write_bytes, flush_bytes, &stream, &rows) == UQA_ERROR_SQL);
        assert(rows == 0);
        if (mode == CANCEL_DATA || mode == CANCEL_FLUSH) {
            sql_error("57014", UQA_OK, NULL);
            CHECK(uqa_engine_reset_cancellation(stream.engine));
        } else if (mode == FAILED_WRITE || mode == FAILED_FLUSH || mode == IGNORED_FAILURE) {
            sql_error("58030", UQA_ERROR_IO, "original callback diagnostic");
        } else {
            sql_error("58030", mode == BARE_FAILURE ? UQA_ERROR_TYPE : UQA_ERROR_INVALID_ARGUMENT, NULL);
        }
        if (mode == FAILED_WRITE) assert(stream.size > 0 && stream.writes == 2 && stream.flushes == 0);
        if (mode == FAILED_FLUSH || mode == CANCEL_FLUSH) assert(stream.size > 0 && stream.flushes == 1);
        unchanged(stream.engine, stream.scratch);
    }

    for (int eof = 0; eof < 2; ++eof) {
        reset(&stream, eof ? CANCEL_EOF : CANCEL_DATA);
        CHECK(uqa_transaction_begin(stream.engine));
        assert(uqa_engine_copy_from(stream.engine, "COPY copy_items FROM STDIN", read_bytes, &stream, &rows) == UQA_ERROR_SQL);
        assert(rows == 0 && stream.reads == 1);
        sql_error("57014", UQA_OK, NULL);
        uint32_t state = UQA_TRANSACTION_IDLE;
        CHECK(uqa_transaction_get_state(stream.engine, &state));
        assert(state == UQA_TRANSACTION_FAILED);
        CHECK(uqa_engine_reset_cancellation(stream.engine));
        CHECK(uqa_transaction_rollback(stream.engine));
        unchanged(stream.engine, stream.scratch);
    }

    reset(&stream, NORMAL);
    rows = 99;
    assert(uqa_engine_copy_to(stream.engine, "COPY (SELECT 1) TO STDOUT", NULL, NULL, NULL, &rows) == UQA_ERROR_INVALID_ARGUMENT);
    assert(rows == 0);
    assert(uqa_engine_copy_from(stream.engine, "COPY copy_items FROM STDIN", NULL, NULL, &rows) == UQA_ERROR_INVALID_ARGUMENT);
    assert(uqa_engine_copy_to_n(stream.engine, "bad\0text", 8, write_bytes, NULL, &stream, &rows) == UQA_ERROR_INVALID_ARGUMENT);
    assert(stream.writes == 0 && stream.reads == 0 && rows == 0);
    CHECK(uqa_engine_copy_to(stream.engine, "COPY (SELECT 7) TO STDOUT", write_bytes, NULL, &stream, &rows));
    assert(rows == 1 && stream.flushes == 0 && strcmp(stream.output, "7\n") == 0);

    CHECK(uqa_result_destroy(&stream.scratch));
    reset(&stream, EXHAUST_DIAGNOSTIC);
    assert(uqa_engine_copy_to(stream.engine, "COPY (SELECT 1) TO STDOUT", write_bytes, NULL, &stream, &rows) == UQA_ERROR_SQL);
    sql_error("58030", UQA_ERROR_IO, "callback failed; diagnostic allocation failed");
    for (size_t index = 0; index < stream.retained_count; ++index) CHECK(uqa_value_destroy(&stream.retained[index]));
    CHECK(uqa_result_destroy(&held));
    CHECK(uqa_result_destroy(&foreign_result));
    CHECK(uqa_cancel_handle_destroy(&stream.cancellation));
    CHECK(uqa_engine_close(&stream.engine));
    CHECK(uqa_engine_close(&stream.other));

    CHECK(uqa_config_create(&config));
    CHECK(uqa_config_set_ffi_memory_limit(config, 1));
    CHECK(uqa_engine_open_memory(config, &stream.engine));
    CHECK(uqa_config_destroy(&config));
    reset(&stream, NORMAL);
    assert(uqa_engine_copy_to(stream.engine, "COPY (SELECT 1) TO STDOUT", write_bytes, NULL, &stream, &rows) == UQA_ERROR_LIMIT);
    assert(rows == 0 && stream.writes == 0);
    CHECK(uqa_engine_close(&stream.engine));
    return 0;
}
