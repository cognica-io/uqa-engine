/*
 * Unified Query Algebra
 *
 * Copyright (c) 2023-2026 Cognica, Inc.
 */

#include "test_database.h"

static void checked(uqa_status status, int line) {
    if (status != UQA_OK) {
        uqa_error error = NULL;
        uqa_string_view message = {NULL, 0};
        if (uqa_error_get_last(&error) == UQA_OK)
            (void)uqa_error_get_message(error, &message);
        fprintf(stderr, "%s, line %d: status %d: %.*s\n", test_database_backend(),
                line, (int)status, (int)message.size, message.data == NULL ? "" : message.data);
        abort();
    }
}
#define CHECK(call) checked((call), __LINE__)

static int64_t number(uqa_result result) {
    uqa_value_view value = NULL;
    int64_t output = 0;
    CHECK(uqa_result_get_value(result, 0, 0, &value));
    CHECK(uqa_value_get_int64(value, &output));
    return output;
}

static void database_path(char *out, size_t size, const char *name) {
    const char *directory = getenv("UQA_TEST_DATABASE_DIR");
    int length;
    assert(directory != NULL);
    length = snprintf(out, size, "%s/%s", directory, name);
    assert(length > 0 && (size_t)length < size);
}

static void sessions_and_reopen(void) {
    char path[4096];
    uqa_config config = NULL;
    uqa_engine first = NULL, second = NULL;
    uqa_result result = NULL, sibling_result = NULL;
    uqa_subscription subscription = NULL;
    uqa_notification notification = NULL;
    const char *channels[] = {"c_storage_events"};
    uqa_subscription_options options = {
        sizeof(options), UQA_SUBSCRIPTION_OPTIONS_VERSION, 2, 1, 8, 8192, 128
    };
    uqa_string_view text = {NULL, 0};
    uint64_t capabilities = 0;
    uint32_t state = 0;
    database_path(path, sizeof(path), "persistent-session");
    CHECK(uqa_config_create(&config));
    CHECK(uqa_config_set_ffi_memory_limit(config, 65536));
    CHECK(test_database_open_path(path, config, &first));
    CHECK(uqa_config_destroy(&config));
    CHECK(uqa_engine_get_capabilities(first, &capabilities));
    assert((capabilities & (UQA_CAP_PERSISTENCE | UQA_CAP_INDEPENDENT_SESSIONS)) ==
           (UQA_CAP_PERSISTENCE | UQA_CAP_INDEPENDENT_SESSIONS));
    CHECK(uqa_result_create(&result));
    CHECK(uqa_result_create(&sibling_result));
    CHECK(uqa_engine_execute(first,
        "CREATE TABLE durable (id integer PRIMARY KEY); INSERT INTO durable VALUES (1)", result));
    CHECK(uqa_engine_open_session(first, &second));
    CHECK(uqa_transaction_begin(first));
    CHECK(uqa_engine_execute(first, "INSERT INTO durable VALUES (2)", result));
    CHECK(uqa_engine_execute(second, "SELECT count(*) FROM durable", sibling_result));
    assert(number(sibling_result) == 1);
    CHECK(uqa_savepoint_create(first, "keep"));
    assert(uqa_engine_execute(first,
        "INSERT INTO durable VALUES (3); SELECT 1/0", result) == UQA_ERROR_SQL);
    CHECK(uqa_transaction_get_state(first, &state));
    assert(state == UQA_TRANSACTION_FAILED);
    CHECK(uqa_savepoint_rollback(first, "keep"));
    CHECK(uqa_savepoint_release(first, "keep"));
    CHECK(uqa_transaction_commit(first));
    CHECK(uqa_engine_execute(second, "SELECT sum(id) FROM durable", sibling_result));
    assert(number(sibling_result) == 3);
    CHECK(uqa_subscription_open(first, channels, 1, &options, &subscription));
    CHECK(uqa_notification_create(&notification));
    assert(uqa_engine_close(&first) == UQA_ERROR_BUSY);
    CHECK(uqa_result_destroy(&result));
    CHECK(uqa_engine_close(&first));
    CHECK(uqa_engine_execute(second, "NOTIFY c_storage_events, 'after close'", sibling_result));
    CHECK(uqa_subscription_poll(subscription, notification));
    CHECK(uqa_notification_get_payload(notification, &text));
    assert(text.size == 11 && memcmp(text.data, "after close", 11) == 0);
    CHECK(uqa_subscription_close(&subscription));
    CHECK(uqa_notification_destroy(&notification));
    CHECK(uqa_result_destroy(&sibling_result));
    CHECK(uqa_engine_close(&second));
    CHECK(test_database_open_path(path, NULL, &first));
    CHECK(uqa_result_create(&result));
    CHECK(uqa_engine_execute(first, "SELECT sum(id) FROM durable", result));
    assert(number(result) == 3);
    CHECK(uqa_transaction_begin(first));
    CHECK(uqa_engine_execute(first, "INSERT INTO durable VALUES (4)", result));
    CHECK(uqa_result_destroy(&result));
    CHECK(uqa_engine_close(&first));
    CHECK(test_database_open_path(path, NULL, &first));
    CHECK(uqa_result_create(&result));
    CHECK(uqa_engine_execute(first, "SELECT sum(id) FROM durable", result));
    assert(number(result) == 3);
    CHECK(uqa_result_destroy(&result));
    CHECK(uqa_engine_close(&first));
}

static void storage_options(void) {
    const char *backend = test_database_backend();
    const uint32_t codecs[] = {UQA_COMPRESSION_NONE, UQA_COMPRESSION_LZ4, UQA_COMPRESSION_ZSTD};
    const int sqlite = strcmp(backend, "sqlite_native") == 0 || strcmp(backend, "sqlite_kv") == 0;
    for (unsigned int encrypted = 0; encrypted < 2; encrypted++) {
        for (unsigned int codec = 0; codec < 3; codec++) {
            char path[4096], name[64];
            uqa_config config = NULL;
            uqa_engine engine = NULL;
            uqa_result result = NULL;
            if (!encrypted && codec == 0) continue;
            (void)snprintf(name, sizeof(name), "options-%u-%u", encrypted, codec);
            database_path(path, sizeof(path), name);
            CHECK(uqa_config_create(&config));
            if (encrypted) CHECK(uqa_config_set_encryption_key(config, "c-abi-test-key"));
            CHECK(uqa_config_set_compression(config, codecs[codec], codec == 2 ? 3 : 0));
            if (!sqlite) {
                assert(test_database_open_path(path, config, &engine) == UQA_ERROR_INVALID_ARGUMENT);
                assert(engine == NULL);
                assert(fopen(path, "rb") == NULL);
            } else {
                CHECK(test_database_open_path(path, config, &engine));
                CHECK(uqa_result_create(&result));
                CHECK(uqa_engine_execute(engine,
                    "CREATE TABLE configured (n integer); INSERT INTO configured VALUES (17)", result));
                CHECK(uqa_result_destroy(&result));
                CHECK(uqa_engine_close(&engine));
                CHECK(test_database_open_path(path, config, &engine));
                CHECK(uqa_result_create(&result));
                CHECK(uqa_engine_execute(engine, "SELECT n FROM configured", result));
                assert(number(result) == 17);
                CHECK(uqa_result_destroy(&result));
                CHECK(uqa_engine_close(&engine));
                if (encrypted && codec == 2) {
                    CHECK(uqa_config_set_encryption_key(config, "wrong-c-abi-test-key"));
                    assert(test_database_open_path(path, config, &engine) == UQA_ERROR_IO);
                    assert(engine == NULL);
                }
            }
            CHECK(uqa_config_destroy(&config));
        }
    }
}

int main(void) {
    if (strcmp(test_database_backend(), "memory") == 0) {
        uqa_engine engine = NULL, sibling = NULL;
        uint64_t capabilities = 0;
        CHECK(test_database_open(NULL, &engine));
        CHECK(uqa_engine_get_capabilities(engine, &capabilities));
        assert((capabilities & (UQA_CAP_PERSISTENCE | UQA_CAP_INDEPENDENT_SESSIONS)) == 0);
        assert(uqa_engine_open_session(engine, &sibling) == UQA_ERROR_UNSUPPORTED);
        assert(sibling == NULL);
        CHECK(uqa_engine_close(&engine));
    } else {
        sessions_and_reopen();
    }
    storage_options();
    return 0;
}
