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
        if (uqa_error_get_last(&error) == UQA_OK)
            (void)uqa_error_get_message(error, &message);
        fprintf(stderr, "line %d: status %d: %.*s\n", line, (int)status,
                (int)message.size, message.data == NULL ? "" : message.data);
        abort();
    }
}
#define CHECK(call) checked((call), __LINE__)

static int equal(uqa_string_view view, const char *text) {
    return view.size == strlen(text) && memcmp(view.data, text, view.size) == 0;
}

static uqa_subscription_options limits(void) {
    uqa_subscription_options options = {
        sizeof(options), UQA_SUBSCRIPTION_OPTIONS_VERSION, 8, 2, 16, 65536, 128
    };
    return options;
}

static void event(uqa_notification value, const char *channel,
                  const char *payload, uint64_t sequence) {
    uqa_string_view text = {NULL, 0};
    uint64_t actual = 0;
    CHECK(uqa_notification_get_channel(value, &text));
    assert(equal(text, channel));
    CHECK(uqa_notification_get_payload(value, &text));
    assert(equal(text, payload));
    CHECK(uqa_notification_get_sequence(value, &actual));
    assert(actual == sequence);
}

static void failure(uqa_notification_failure_kind kind, const char *code) {
    uqa_error error = NULL;
    uqa_notification_failure_kind actual = 0;
    uqa_string_view text = {NULL, 0};
    uqa_status status = 0;
    CHECK(uqa_error_get_last(&error));
    CHECK(uqa_error_get_status(error, &status));
    assert(status == UQA_ERROR_NOTIFICATION);
    CHECK(uqa_error_get_notification_kind(error, &actual));
    assert(actual == kind);
    CHECK(uqa_error_get_notification_code(error, &text));
    assert(equal(text, code));
    CHECK(uqa_error_get_message(error, &text));
    assert(equal(text, code));
    assert(uqa_error_get_sqlstate(error, &text) == UQA_DONE);
    assert(text.data == NULL && text.size == 0);
}

static void independent_and_session_delivery(void) {
    uqa_engine engine = NULL;
    uqa_result result = NULL;
    uqa_subscription first = NULL, second = NULL;
    uqa_notification value = NULL, other = NULL, sql_value = NULL;
    uqa_subscription_options options = limits();
    uqa_sql_notification_options sql_options = {
        sizeof(sql_options), UQA_SQL_NOTIFICATION_OPTIONS_VERSION, 2, 16, 65536, 128
    };
    const char *channels[] = {"jobs", "results"};
    const char *spans[] = {"jobs ignored", "results ignored"};
    size_t sizes[] = {4, 7};
    uqa_string_view text = {NULL, 0};
    uqa_bytes_view epoch = {NULL, 0};
    uint8_t identity[16];
    uqa_value_view pid_value = NULL;
    int64_t sender = 0;
    int32_t actual_sender = 0;
    uint32_t state = UQA_TRANSACTION_IDLE;
    CHECK(test_database_open(NULL, &engine));
    CHECK(uqa_result_create(&result));
    CHECK(uqa_notification_create(&value));
    CHECK(uqa_notification_create(&other));
    CHECK(uqa_notification_create(&sql_value));
    assert(uqa_notification_get_payload(value, &text) == UQA_ERROR_STATE);
    assert(text.data == NULL && text.size == 0);
    CHECK(uqa_engine_configure_sql_notifications(engine, &sql_options));
    assert(uqa_engine_configure_sql_notifications(engine, &sql_options) == UQA_ERROR_NOTIFICATION);
    CHECK(uqa_engine_execute(engine, "LISTEN jobs; LISTEN results; SELECT pg_backend_pid()", result));
    CHECK(uqa_result_get_value(result, 0, 0, &pid_value));
    CHECK(uqa_value_get_int64(pid_value, &sender));
    assert(sender > 0);
    CHECK(uqa_engine_execute(engine, "BEGIN", result));
    CHECK(uqa_subscription_open(engine, channels, 2, &options, &first));
    CHECK(uqa_subscription_open_n(engine, spans, sizes, 2, &options, &second));
    CHECK(uqa_transaction_get_state(engine, &state));
    assert(state == UQA_TRANSACTION_ACTIVE);
    /* PostgreSQL reference: notification_session_oracle and rule_notify_oracle. */
    CHECK(uqa_engine_execute(engine,
        "NOTIFY jobs, 'one'; NOTIFY jobs, 'one'; SAVEPOINT keep; "
        "NOTIFY jobs, 'discard'; ROLLBACK TO keep; NOTIFY results, 'two'", result));
    assert(uqa_subscription_poll(first, value) == UQA_TIMEOUT);
    assert(uqa_engine_poll_sql_notification(engine, sql_value) == UQA_TIMEOUT);
    CHECK(uqa_engine_execute(engine, "COMMIT", result));
    CHECK(uqa_subscription_wait(first, 1000, value));
    event(value, "jobs", "one", 1);
    CHECK(uqa_notification_get_sender_pid(value, &actual_sender));
    assert(actual_sender == sender);
    CHECK(uqa_notification_get_epoch(value, &epoch));
    assert(epoch.size == sizeof(identity));
    memcpy(identity, epoch.data, sizeof(identity));
    CHECK(uqa_subscription_poll(second, other));
    event(other, "jobs", "one", 1);
    CHECK(uqa_notification_get_epoch(other, &epoch));
    assert(epoch.size == sizeof(identity) && memcmp(identity, epoch.data, sizeof(identity)) != 0);
    CHECK(uqa_subscription_poll(first, value));
    event(value, "results", "two", 2);
    CHECK(uqa_notification_get_epoch(value, &epoch));
    assert(memcmp(identity, epoch.data, sizeof(identity)) == 0);
    assert(uqa_subscription_wait(first, 0, value) == UQA_TIMEOUT);
    event(value, "results", "two", 2);
    CHECK(uqa_subscription_poll(second, other));
    CHECK(uqa_engine_execute(engine, "BEGIN", result));
    assert(uqa_engine_poll_sql_notification(engine, sql_value) == UQA_TIMEOUT);
    CHECK(uqa_engine_execute(engine, "COMMIT", result));
    CHECK(uqa_engine_poll_sql_notification(engine, sql_value));
    event(sql_value, "jobs", "one", 1);
    CHECK(uqa_engine_poll_sql_notification(engine, sql_value));
    event(sql_value, "results", "two", 2);
    CHECK(uqa_engine_execute(engine, "BEGIN; NOTIFY jobs, 'rollback'; ROLLBACK", result));
    assert(uqa_subscription_poll(first, value) == UQA_TIMEOUT);
    assert(uqa_engine_poll_sql_notification(engine, sql_value) == UQA_TIMEOUT);
    CHECK(uqa_engine_execute(engine, "UNLISTEN *; NOTIFY jobs, 'after unlisten'", result));
    assert(uqa_engine_poll_sql_notification(engine, sql_value) == UQA_TIMEOUT);
    CHECK(uqa_result_destroy(&result));
    CHECK(uqa_engine_close(&engine));
    CHECK(uqa_subscription_poll(first, value));
    event(value, "jobs", "after unlisten", 3);
    CHECK(uqa_subscription_close(&first));
    CHECK(uqa_subscription_close(&second));
    CHECK(uqa_subscription_close(&first));
    CHECK(uqa_subscription_close(NULL));
    event(value, "jobs", "after unlisten", 3);
    event(sql_value, "results", "two", 2);
    CHECK(uqa_notification_destroy(&value));
    CHECK(uqa_notification_destroy(&value));
    CHECK(uqa_notification_destroy(NULL));
    CHECK(uqa_notification_destroy(&other));
    CHECK(uqa_notification_destroy(&sql_value));
}

static void validation_and_terminal_failures(void) {
    uqa_engine engine = NULL;
    uqa_result result = NULL;
    uqa_subscription subscription = NULL, rejected = NULL;
    uqa_notification value = NULL;
    uqa_subscription_options options = limits();
    const char *channels[] = {"jobs", "jobs"};
    struct { uint32_t struct_size; uint32_t version; } prefix = {8, 1};
    struct { uqa_subscription_options known; uint64_t extension; } extended = {limits(), 0};
    CHECK(test_database_open(NULL, &engine));
    CHECK(uqa_result_create(&result));
    CHECK(uqa_notification_create(&value));
    assert(uqa_subscription_open(engine, channels, 1, NULL, &rejected) == UQA_ERROR_INVALID_ARGUMENT);
    assert(uqa_subscription_open(engine, channels, 1,
        (const uqa_subscription_options *)(const void *)&prefix, &rejected) == UQA_ERROR_INVALID_ARGUMENT);
    options.version = 2;
    assert(uqa_subscription_open(engine, channels, 1, &options, &rejected) == UQA_ERROR_INVALID_ARGUMENT);
    options = limits();
    options.max_channels = 0;
    assert(uqa_subscription_open(engine, channels, 1, &options, &rejected) == UQA_ERROR_INVALID_ARGUMENT);
    options = limits();
    assert(uqa_subscription_open(engine, channels, 2, &options, &rejected) == UQA_ERROR_NOTIFICATION);
    failure(UQA_NOTIFICATION_FAILURE_INVALID_REQUEST, "NOTIFICATION_INVALID_REQUEST");
    extended.known.struct_size = sizeof(extended);
    extended.extension = 1;
    assert(uqa_subscription_open(engine, channels, 1, &extended.known, &rejected) == UQA_ERROR_INVALID_ARGUMENT);
    extended.extension = 0;
    CHECK(uqa_subscription_open(engine, channels, 1, &extended.known, &subscription));
    assert(uqa_subscription_open(engine, channels, 1, &options, &subscription) == UQA_ERROR_INVALID_ARGUMENT);
    options.max_active_subscriptions = 1;
    assert(uqa_subscription_open(engine, channels, 1, &options, &rejected) == UQA_ERROR_NOTIFICATION);
    failure(UQA_NOTIFICATION_FAILURE_CAPACITY, "NOTIFICATION_CAPACITY");
    assert(rejected == NULL);
    CHECK(uqa_subscription_close(&subscription));
    options.max_queued_notifications = 1;
    CHECK(uqa_subscription_open(engine, channels, 1, &options, &subscription));
    CHECK(uqa_engine_execute(engine, "NOTIFY jobs, 'kept'", result));
    CHECK(uqa_subscription_poll(subscription, value));
    CHECK(uqa_engine_execute(engine, "NOTIFY jobs, 'private one'; NOTIFY jobs, 'private two'", result));
    assert(uqa_subscription_poll(subscription, value) == UQA_ERROR_NOTIFICATION);
    failure(UQA_NOTIFICATION_FAILURE_BACKPRESSURE, "NOTIFICATION_BACKPRESSURE");
    event(value, "jobs", "kept", 1);
    assert(uqa_subscription_wait(subscription, 0, value) == UQA_ERROR_NOTIFICATION);
    failure(UQA_NOTIFICATION_FAILURE_BACKPRESSURE, "NOTIFICATION_BACKPRESSURE");
    CHECK(uqa_subscription_close(&subscription));
    CHECK(uqa_result_destroy(&result));
    CHECK(uqa_engine_close(&engine));
    CHECK(uqa_notification_destroy(&value));
}

static uqa_status receive(uqa_engine engine, uqa_subscription subscription,
                          uqa_notification value) {
    return subscription == NULL ? uqa_engine_poll_sql_notification(engine, value)
                                : uqa_subscription_poll(subscription, value);
}

static void failed_admission_preserves_delivery(int use_sql) {
    uqa_config config = NULL;
    uqa_engine engine = NULL;
    uqa_result result = NULL;
    uqa_subscription subscription = NULL;
    uqa_notification value = NULL;
    uqa_value_view source = NULL;
    uqa_value retained[128] = {NULL};
    size_t filled = 0;
    const char *channels[] = {"jobs"};
    uqa_subscription_options options = limits();
    uqa_sql_notification_options sql_options = {
        sizeof(sql_options), UQA_SQL_NOTIFICATION_OPTIONS_VERSION, 1, 16, 65536, 128
    };
    uqa_string_view payload = {NULL, 0};
    uint64_t sequence = 0;
    CHECK(uqa_config_create(&config));
    CHECK(uqa_config_set_ffi_memory_limit(config, 8192));
    CHECK(test_database_open(config, &engine));
    CHECK(uqa_config_destroy(&config));
    CHECK(uqa_result_create(&result));
    CHECK(uqa_notification_create(&value));
    if (use_sql) {
        CHECK(uqa_engine_configure_sql_notifications(engine, &sql_options));
        CHECK(uqa_engine_execute(engine, "LISTEN jobs", result));
    } else {
        CHECK(uqa_subscription_open(engine, channels, 1, &options, &subscription));
    }
    CHECK(uqa_engine_execute(engine, "NOTIFY jobs, 'first'", result));
    CHECK(receive(engine, subscription, value));
    CHECK(uqa_engine_execute(engine,
        "SELECT pg_notify('jobs', repeat('p', 512)); SELECT 'held'::text", result));
    CHECK(uqa_result_get_value(result, 0, 0, &source));
    for (; filled < 128; ++filled) {
        uqa_status status = uqa_value_create_copy(source, &retained[filled]);
        if (status == UQA_ERROR_LIMIT) break;
        CHECK(status);
    }
    assert(filled > 0 && filled < 128);
    assert(receive(engine, subscription, value) == UQA_ERROR_LIMIT);
    event(value, "jobs", "first", 1);
    for (size_t index = 0; index < filled; ++index) CHECK(uqa_value_destroy(&retained[index]));
    CHECK(uqa_result_destroy(&result));
    if (!use_sql) CHECK(uqa_engine_close(&engine));
    CHECK(receive(engine, subscription, value));
    CHECK(uqa_notification_get_payload(value, &payload));
    assert(payload.size == 512 && payload.data[0] == 'p' && payload.data[511] == 'p');
    CHECK(uqa_notification_get_sequence(value, &sequence));
    assert(sequence == 2);
    assert(receive(engine, subscription, value) == UQA_TIMEOUT);
    CHECK(uqa_subscription_close(&subscription));
    CHECK(uqa_engine_close(&engine));
    CHECK(uqa_notification_destroy(&value));
}

static void private_detail_and_sql_failure_retention(void) {
    for (int use_sql = 0; use_sql < 2; ++use_sql) {
        uqa_engine engine = NULL;
        uqa_result result = NULL;
        uqa_notification value = NULL;
        uqa_subscription subscription = NULL;
        uqa_subscription_options options = limits();
        uqa_sql_notification_options sql_options = {
            sizeof(sql_options), UQA_SQL_NOTIFICATION_OPTIONS_VERSION, 1, 16, 1, 128
        };
        const char *channels[] = {"jobs"};
        uqa_error original = NULL, again = NULL;
        uqa_string_view detail = {NULL, 0};
        options.max_queued_bytes = 1;
        CHECK(test_database_open(NULL, &engine));
        CHECK(uqa_result_create(&result));
        CHECK(uqa_notification_create(&value));
        assert(uqa_engine_poll_sql_notification(engine, value) == UQA_ERROR_NOTIFICATION);
        failure(UQA_NOTIFICATION_FAILURE_INVALID_REQUEST, "NOTIFICATION_INVALID_REQUEST");
        if (use_sql) {
            CHECK(uqa_engine_configure_sql_notifications(engine, &sql_options));
            CHECK(uqa_engine_execute(engine, "LISTEN jobs", result));
        } else {
            CHECK(uqa_subscription_open(engine, channels, 1, &options, &subscription));
        }
        CHECK(uqa_engine_execute(engine, "NOTIFY jobs, 'private payload'", result));
        assert(receive(engine, subscription, value) == UQA_ERROR_NOTIFICATION);
        failure(UQA_NOTIFICATION_FAILURE_BACKPRESSURE, "NOTIFICATION_BACKPRESSURE");
        CHECK(uqa_error_get_last(&original));
        CHECK(uqa_error_get_detail(original, &detail));
        assert(detail.size > 0 && !equal(detail, "NOTIFICATION_BACKPRESSURE"));
        CHECK(uqa_result_destroy(&result));
        assert(receive(engine, subscription, value) == UQA_ERROR_NOTIFICATION);
        CHECK(uqa_error_get_last(&again));
        /* A later receive republishes the original retained record after TLS was cleared. */
        assert(again == original);
        failure(UQA_NOTIFICATION_FAILURE_BACKPRESSURE, "NOTIFICATION_BACKPRESSURE");
        CHECK(uqa_subscription_close(&subscription));
        CHECK(uqa_engine_close(&engine));
        CHECK(uqa_notification_destroy(&value));
    }
}

int main(void) {
    independent_and_session_delivery();
    validation_and_terminal_failures();
    failed_admission_preserves_delivery(0);
    failed_admission_preserves_delivery(1);
    private_detail_and_sql_failure_retention();
    return 0;
}
