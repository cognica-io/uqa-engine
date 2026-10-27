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

static void fields_and_order(void) {
    uqa_engine engine = NULL;
    uqa_result result = NULL;
    uqa_notice first = NULL, second = NULL;
    uqa_string_view text = {NULL, 0};
    uqa_notice_level level = 99;
    CHECK(test_database_open(NULL, &engine));
    CHECK(uqa_result_create(&result));
    assert(uqa_notice_create(engine, &first) == UQA_DONE && first == NULL);
    /* Independent reference: tests/parity/pg18/raise_options_oracle.expected.json. */
    CHECK(uqa_engine_execute(engine,
        "DO $$ BEGIN RAISE NOTICE USING MESSAGE='notice', DETAIL='why', HINT='hint', ERRCODE='01001'; END $$; "
        "DO $$ BEGIN RAISE WARNING USING HINT='hint only'; END $$", result));
    CHECK(uqa_notice_create(engine, &first));
    assert(uqa_notice_create(engine, &first) == UQA_ERROR_INVALID_ARGUMENT);
    CHECK(uqa_notice_get_level(first, &level));
    assert(level == UQA_NOTICE_LEVEL_NOTICE);
    CHECK(uqa_notice_get_sqlstate(first, &text));
    assert(equal(text, "01001"));
    CHECK(uqa_notice_get_message(first, &text));
    assert(equal(text, "notice"));
    CHECK(uqa_notice_get_detail(first, &text));
    assert(equal(text, "why"));
    CHECK(uqa_notice_get_hint(first, &text));
    assert(equal(text, "hint"));
    CHECK(uqa_notice_create(engine, &second));
    CHECK(uqa_notice_get_level(second, &level));
    assert(level == UQA_NOTICE_LEVEL_WARNING);
    CHECK(uqa_notice_get_sqlstate(second, &text));
    assert(equal(text, "01000"));
    CHECK(uqa_notice_get_message(second, &text));
    assert(equal(text, "00000"));
    assert(uqa_notice_get_detail(second, &text) == UQA_DONE);
    assert(text.data == NULL && text.size == 0);
    CHECK(uqa_notice_get_hint(second, &text));
    assert(equal(text, "hint only"));
    CHECK(uqa_notice_destroy(&first));
    assert(uqa_notice_create(engine, &first) == UQA_DONE && first == NULL);
    CHECK(uqa_notice_destroy(&second));
    CHECK(uqa_engine_execute(engine,
        "SET client_min_messages = warning; "
        "DO $$ BEGIN RAISE NOTICE 'filtered'; RAISE INFO 'always'; END $$; "
        "SET client_min_messages = debug5; "
        "DO $$ BEGIN RAISE DEBUG 'debug'; RAISE LOG 'log'; END $$", result));
    CHECK(uqa_notice_create(engine, &first));
    CHECK(uqa_notice_get_level(first, &level));
    assert(level == UQA_NOTICE_LEVEL_INFO);
    CHECK(uqa_notice_get_message(first, &text));
    assert(equal(text, "always"));
    CHECK(uqa_notice_destroy(&first));
    CHECK(uqa_notice_create(engine, &first));
    CHECK(uqa_notice_get_level(first, &level));
    assert(level == UQA_NOTICE_LEVEL_DEBUG);
    CHECK(uqa_notice_get_message(first, &text));
    assert(equal(text, "debug"));
    CHECK(uqa_notice_destroy(&first));
    CHECK(uqa_notice_create(engine, &first));
    CHECK(uqa_notice_get_level(first, &level));
    assert(level == UQA_NOTICE_LEVEL_LOG);
    CHECK(uqa_notice_get_message(first, &text));
    assert(equal(text, "log"));
    CHECK(uqa_result_destroy(&result));
    CHECK(uqa_engine_close(&engine));
    CHECK(uqa_notice_get_message(first, &text));
    assert(equal(text, "log"));
    CHECK(uqa_notice_destroy(&first));
    CHECK(uqa_notice_destroy(&first));
    CHECK(uqa_notice_destroy(NULL));
}

static void failed_admission_preserves_the_queue(void) {
    uqa_config config = NULL;
    uqa_engine engine = NULL;
    uqa_result result = NULL;
    uqa_value_view value = NULL;
    uqa_value retained[64] = {NULL};
    uqa_notice first = NULL, second = NULL;
    uqa_string_view message = {NULL, 0};
    size_t filled = 0;
    CHECK(uqa_config_create(&config));
    CHECK(uqa_config_set_ffi_memory_limit(config, 4096));
    CHECK(test_database_open(config, &engine));
    CHECK(uqa_config_destroy(&config));
    CHECK(uqa_result_create(&result));
    CHECK(uqa_engine_execute(engine,
        "DO $$ BEGIN RAISE NOTICE USING MESSAGE=repeat('n', 512); RAISE NOTICE 'tail'; END $$; SELECT 'held'::text", result));
    CHECK(uqa_result_get_value(result, 0, 0, &value));
    for (; filled < 64; ++filled) {
        uqa_status status = uqa_value_create_copy(value, &retained[filled]);
        if (status == UQA_ERROR_LIMIT) break;
        CHECK(status);
    }
    assert(filled > 0 && filled < 64);
    assert(uqa_notice_create(engine, &first) == UQA_ERROR_LIMIT && first == NULL);
    for (size_t index = 0; index < filled; ++index) CHECK(uqa_value_destroy(&retained[index]));
    CHECK(uqa_result_destroy(&result));
    CHECK(uqa_notice_create(engine, &first));
    CHECK(uqa_notice_get_message(first, &message));
    assert(message.size == 512 && message.data[0] == 'n' && message.data[511] == 'n');
    CHECK(uqa_notice_create(engine, &second));
    CHECK(uqa_notice_get_message(second, &message));
    assert(equal(message, "tail"));
    CHECK(uqa_notice_destroy(&second));
    assert(uqa_notice_create(engine, &second) == UQA_DONE && second == NULL);
    CHECK(uqa_engine_close(&engine));
    CHECK(uqa_notice_get_message(first, &message));
    assert(message.size == 512 && message.data[511] == 'n');
    CHECK(uqa_notice_destroy(&first));
}

int main(void) {
    fields_and_order();
    failed_admission_preserves_the_queue();
    return 0;
}
