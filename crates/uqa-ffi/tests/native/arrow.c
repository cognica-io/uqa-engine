/*
 * Unified Query Algebra
 *
 * Copyright (c) 2023-2026 Cognica, Inc.
 */

#include "test_database.h"
#include <uqa/uqa_arrow.h>

static void checked(uqa_status status, int line) {
    if (status != UQA_OK) {
        uqa_error error = NULL;
        uqa_string_view text;
        memset(&text, 0, sizeof(text));
        if (uqa_error_get_last(&error) == UQA_OK)
            (void)uqa_error_get_message(error, &text);
        fprintf(stderr, "arrow %s:%d status %d: %.*s\n", test_database_backend(), line,
                (int)status, (int)text.size, text.data == NULL ? "" : text.data);
        abort();
    }
}
#define CHECK(call) checked((call), __LINE__)

static void release(struct ArrowSchema *schema, struct ArrowArray *array) {
    if (schema->release != NULL) schema->release(schema);
    if (array->release != NULL) array->release(array);
    assert(schema->release == NULL && array->release == NULL);
    memset(schema, 0, sizeof(*schema));
    memset(array, 0, sizeof(*array));
}

static int32_t integer(const struct ArrowArray *array, int64_t row) {
    int32_t value;
    assert(array->n_buffers == 2 && array->buffers[1] != NULL);
    memcpy(&value, (const unsigned char *)array->buffers[1] +
                  (size_t)(array->offset + row) * sizeof(value), sizeof(value));
    return value;
}

static void utf8(const struct ArrowArray *array, const char *expected) {
    const int64_t *offsets = (const int64_t *)array->buffers[1];
    const char *bytes = (const char *)array->buffers[2];
    assert(array->n_buffers == 3);
    assert((size_t)(offsets[array->offset + 1] - offsets[array->offset]) == strlen(expected));
    assert(memcmp(bytes + offsets[array->offset], expected, strlen(expected)) == 0);
}

static void detached_child_keeps_its_allowance(void) {
    uqa_config config = NULL;
    uqa_engine engine = NULL;
    uqa_result result = NULL;
    struct ArrowSchema schema, again_schema, child_schema;
    struct ArrowArray array, again_array, child_array;
    memset(&schema, 0, sizeof(schema));
    memset(&array, 0, sizeof(array));
    memset(&again_schema, 0, sizeof(again_schema));
    memset(&again_array, 0, sizeof(again_array));
    CHECK(uqa_config_create(&config));
    CHECK(uqa_config_set_ffi_memory_limit(config, 300000));
    CHECK(test_database_open(config, &engine));
    CHECK(uqa_config_destroy(&config));
    CHECK(uqa_result_create(&result));
    CHECK(uqa_engine_execute(engine, "SELECT repeat('x', 100000)::text AS text", result));
    CHECK(uqa_result_export_arrow(result, UQA_ARROW_LOSSLESS, &schema, &array));
    child_schema = *schema.children[0];
    child_array = *array.children[0];
    memset(schema.children[0], 0, sizeof(*schema.children[0]));
    memset(array.children[0], 0, sizeof(*array.children[0]));
    release(&schema, &array);
    assert(strcmp(child_schema.format, "U") == 0);
    assert(child_array.length == 1);
    assert(uqa_result_export_arrow(result, UQA_ARROW_LOSSLESS, &again_schema, &again_array) < 0);
    assert(again_schema.release == NULL && again_array.release == NULL);
    child_array.release(&child_array);
    /* A schema child alone must not keep the array's payload allowance. */
    CHECK(uqa_result_export_arrow(result, UQA_ARROW_LOSSLESS, &again_schema, &again_array));
    CHECK(uqa_result_destroy(&result));
    CHECK(uqa_engine_close(&engine));
    child_schema.release(&child_schema);
    release(&again_schema, &again_array);
}

int main(void) {
    uqa_engine engine = NULL;
    uqa_result result = NULL;
    uqa_cursor cursor = NULL;
    uint64_t capabilities = 0, count = 0;
    struct ArrowSchema schema, moved_schema;
    struct ArrowArray array, moved_array;
    memset(&schema, 0, sizeof(schema));
    memset(&array, 0, sizeof(array));
    CHECK(test_database_open(NULL, &engine));
    CHECK(uqa_result_create(&result));
    CHECK(uqa_library_get_capabilities(&capabilities));
    CHECK(uqa_engine_execute(engine, "SELECT 7::smallint AS x, 8::integer AS x, NULL::bigint AS n", result));
    assert(uqa_result_export_arrow(result, UINT32_MAX, &schema, &array) == UQA_ERROR_INVALID_ARGUMENT);
    assert(schema.release == NULL && array.release == NULL);
    if ((capabilities & UQA_CAP_ARROW_EXPORT) == 0) {
        assert(uqa_result_export_arrow(result, UQA_ARROW_LOSSLESS, &schema, &array) == UQA_ERROR_UNSUPPORTED);
        CHECK(uqa_cursor_open(engine, "SELECT 9 AS v", &cursor));
        assert(uqa_cursor_fetch_arrow(cursor, 1, UQA_ARROW_LOSSLESS, &schema, &array) == UQA_ERROR_UNSUPPORTED);
        assert(schema.release == NULL && array.release == NULL);
        CHECK(uqa_cursor_fetch(cursor, 1, result));
        CHECK(uqa_result_get_row_count(result, &count));
        assert(count == 1);
        CHECK(uqa_cursor_close(&cursor));
        CHECK(uqa_result_destroy(&result));
        CHECK(uqa_engine_close(&engine));
        return 0;
    }

    CHECK(uqa_result_export_arrow(result, UQA_ARROW_LOSSLESS, &schema, &array));
    assert(strcmp(schema.format, "+s") == 0 && schema.n_children == 3);
    assert(array.length == 1 && array.n_children == 3);
    assert(strcmp(schema.children[0]->format, "s") == 0);
    assert(strcmp(schema.children[1]->format, "i") == 0);
    assert(strcmp(schema.children[0]->name, "x") == 0);
    assert(strcmp(schema.children[1]->name, "x") == 0);
    assert(array.children[2]->null_count == 1);
    assert(integer(array.children[1], 0) == 8);
    assert(uqa_result_export_arrow(result, UQA_ARROW_LOSSLESS, &schema, &array) == UQA_ERROR_STATE);
    assert(integer(array.children[1], 0) == 8);
    moved_schema = schema;
    moved_array = array;
    memset(&schema, 0, sizeof(schema));
    memset(&array, 0, sizeof(array));
    moved_schema.release(&moved_schema);
    assert(moved_schema.release == NULL);
    CHECK(uqa_engine_execute(engine, "SELECT 42 AS replacement", result));
    assert(integer(moved_array.children[1], 0) == 8);
    moved_array.release(&moved_array);
    assert(moved_array.release == NULL);

    CHECK(uqa_engine_execute(engine, "SELECT 1::integer AS x WHERE FALSE", result));
    CHECK(uqa_result_export_arrow(result, UQA_ARROW_LOSSLESS, &schema, &array));
    assert(array.length == 0 && schema.n_children == 1);
    assert(strcmp(schema.children[0]->format, "i") == 0);
    release(&schema, &array);

    CHECK(uqa_engine_execute(engine, "SELECT 1.25::numeric AS n", result));
    assert(uqa_result_export_arrow(result, UQA_ARROW_LOSSLESS, &schema, &array) == UQA_ERROR_UNSUPPORTED);
    assert(schema.release == NULL && array.release == NULL);
    CHECK(uqa_result_export_arrow(result, UQA_ARROW_SQL_TEXT, &schema, &array));
    utf8(array.children[0], "1.25");
    assert(strcmp(schema.children[0]->format, "U") == 0);
    release(&schema, &array);

    CHECK(uqa_cursor_open(engine, "SELECT n::integer AS n FROM generate_series(1, 5) AS g(n)", &cursor));
    assert(uqa_cursor_fetch_arrow(cursor, 0, UQA_ARROW_LOSSLESS, &schema, &array) == UQA_ERROR_INVALID_ARGUMENT);
    for (int32_t next = 1; next <= 5;) {
        CHECK(uqa_cursor_fetch_arrow(cursor, 2, UQA_ARROW_LOSSLESS, &schema, &array));
        assert(array.length > 0 && array.length <= 2);
        for (int64_t i = 0; i < array.length; i++) assert(integer(array.children[0], i) == next++);
        release(&schema, &array);
    }
    assert(uqa_cursor_fetch_arrow(cursor, 2, UQA_ARROW_LOSSLESS, &schema, &array) == UQA_DONE);
    assert(schema.release == NULL && array.release == NULL);
    CHECK(uqa_cursor_close(&cursor));

    CHECK(uqa_cursor_open(engine, "SELECT 1.25::numeric AS n", &cursor));
    assert(uqa_cursor_fetch_arrow(cursor, 1, UQA_ARROW_LOSSLESS, &schema, &array) == UQA_ERROR_UNSUPPORTED);
    assert(uqa_cursor_fetch_arrow(cursor, 1, UQA_ARROW_SQL_TEXT, &schema, &array) == UQA_ERROR_UNSUPPORTED);
    assert(uqa_cursor_fetch(cursor, 1, result) == UQA_ERROR_UNSUPPORTED);
    assert(schema.release == NULL && array.release == NULL);
    CHECK(uqa_cursor_close(&cursor));

    CHECK(uqa_engine_execute(engine, "SELECT 'kept'::text AS s", result));
    CHECK(uqa_result_export_arrow(result, UQA_ARROW_LOSSLESS, &schema, &array));
    CHECK(uqa_result_destroy(&result));
    CHECK(uqa_engine_close(&engine));
    utf8(array.children[0], "kept");
    array.release(&array);
    assert(array.release == NULL);
    assert(strcmp(schema.children[0]->name, "s") == 0);
    schema.release(&schema);
    assert(schema.release == NULL);
    detached_child_keeps_its_allowance();
    return 0;
}
