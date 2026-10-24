/*
 * Unified Query Algebra
 *
 * Copyright (c) 2023-2026 Cognica, Inc.
 */

#include "test_udf.h"
#include <pthread.h>

enum mode { IDENTITY, CONSTANT, NULL_RESULT, MISSING, FAIL, NEGATIVE, POSITIVE, REENTRY, BLOCK, LIMIT_RESULT, CANCEL_RESULT, CANCEL_ASSIGNMENT };
struct data {
    enum mode mode;
    int calls, destroyed, fail_destroy;
    int64_t number;
    uqa_engine engine, other;
    uqa_udf *handle;
    uqa_cancel_handle cancellation;
    pthread_mutex_t mutex;
    pthread_cond_t condition;
    int entered, released;
};

static uqa_status UQA_CALL scalar(uqa_udf_context context, uqa_value_view const *arguments,
                                  uint32_t count, void *user_data) {
    struct data *data = (struct data *)user_data;
    uqa_value_view result = NULL;
    ++data->calls;
    assert(uqa_udf_context_get_result(context, &result) == UQA_DONE && result == NULL);
    switch (data->mode) {
        case IDENTITY: {
            assert(count == 1);
            uqa_type type = NULL;
            uint8_t is_null = 0;
            CHECK(uqa_value_get_is_null(arguments[0], &is_null));
            if (is_null) assert(uqa_value_get_type(arguments[0], &type) == UQA_DONE && type == NULL);
            else CHECK(uqa_value_get_type(arguments[0], &type));
            CHECK(uqa_udf_context_set_result(context, arguments[0]));
            CHECK(uqa_udf_context_get_result(context, &result));
            assert(result != NULL);
            assert(uqa_udf_context_set_result(context, arguments[0]) == UQA_ERROR_STATE);
            uqa_value_view preserved = NULL;
            CHECK(uqa_udf_context_get_result(context, &preserved));
            assert(preserved == result);
            return UQA_OK;
        }
        case NULL_RESULT: {
            uqa_value value = NULL;
            uint8_t is_null = 0;
            CHECK(uqa_value_create_null(&value));
            CHECK(uqa_udf_context_set_result(context, value));
            CHECK(uqa_value_destroy(&value));
            CHECK(uqa_udf_context_get_result(context, &result));
            CHECK(uqa_value_get_is_null(result, &is_null));
            assert(is_null == 1);
            return UQA_OK;
        }
        case MISSING: return UQA_OK;
        case FAIL: {
            char message[] = "scalar original failure";
            assert(uqa_udf_context_fail(context, message) == UQA_ERROR_SQL);
            message[0] = 'x';
            assert(uqa_callback_fail(UQA_ERROR_IO, "later failure") == UQA_ERROR_SQL);
            /* Ordinary successful helpers may clear thread diagnostics. */
            uqa_value value = NULL;
            CHECK(uqa_value_create_int64(9, &value));
            CHECK(uqa_value_destroy(&value));
            return UQA_OK;
        }
        case NEGATIVE: return UQA_ERROR_IO;
        case POSITIVE: return UQA_DONE;
        case REENTRY: {
            uqa_result nested = NULL;
            CHECK(uqa_result_create(&nested));
            assert(uqa_engine_execute(data->engine, "SELECT 1", nested) == UQA_ERROR_BUSY);
            assert(uqa_engine_execute(data->other, "SELECT 1", nested) == UQA_ERROR_BUSY);
            assert(uqa_udf_unregister(data->handle) == UQA_ERROR_BUSY);
            uqa_udf rejected = NULL;
            assert(uqa_udf_register_scalar(data->other, "nested", NULL, scalar, data, NULL, &rejected) == UQA_ERROR_BUSY);
            assert(rejected == NULL);
            CHECK(uqa_result_destroy(&nested));
            return udf_set_int(context, 81);
        }
        case BLOCK:
            assert(pthread_mutex_lock(&data->mutex) == 0);
            data->entered = 1;
            assert(pthread_cond_broadcast(&data->condition) == 0);
            while (!data->released) assert(pthread_cond_wait(&data->condition, &data->mutex) == 0);
            assert(pthread_mutex_unlock(&data->mutex) == 0);
            return udf_set_int(context, 92);
        case CONSTANT: return udf_set_int(context, data->number);
        case LIMIT_RESULT: {
            char text[65536];
            memset(text, 'x', sizeof(text));
            uqa_value value = NULL;
            CHECK(uqa_value_create_text_n(text, sizeof(text), &value));
            assert(uqa_udf_context_set_result(context, value) == UQA_ERROR_LIMIT);
            assert(uqa_udf_context_get_result(context, &result) == UQA_DONE && result == NULL);
            CHECK(uqa_value_destroy(&value));
            return udf_set_int(context, 51);
        }
        case CANCEL_RESULT:
            CHECK(udf_set_int(context, 13));
            CHECK(uqa_cancel_handle_request(data->cancellation));
            return UQA_OK;
        case CANCEL_ASSIGNMENT:
            CHECK(uqa_cancel_handle_request(data->cancellation));
            return udf_set_int(context, 13);
    }
    abort();
}

static uqa_status UQA_CALL destroy_data(void *user_data) {
    struct data *data = (struct data *)user_data;
    ++data->destroyed;
    assert(data->destroyed == 1);
    return data->fail_destroy ? uqa_callback_fail(UQA_ERROR_IO, "user-data cleanup failed") : UQA_OK;
}

static void *execute_blocked(void *user_data) {
    struct data *data = (struct data *)user_data;
    uqa_result result = NULL;
    CHECK(uqa_result_create(&result));
    CHECK(uqa_engine_execute(data->engine, "SELECT c_scalar(1)", result));
    assert(udf_result_int(result, 0, 0) == 92);
    CHECK(uqa_result_destroy(&result));
    return NULL;
}

int main(void) {
    uqa_engine engine = NULL, other = NULL;
    uqa_result result = NULL;
    uqa_udf function = NULL;
    uqa_udf_options options = NULL;
    struct data data;
    memset(&data, 0, sizeof(data));
    CHECK(test_database_open(NULL, &engine));
    CHECK(test_database_open(NULL, &other));
    CHECK(uqa_result_create(&result));
    CHECK(uqa_udf_options_create(&options));
    CHECK(uqa_udf_options_set_may_mutate_engine(options, 0));
    data.mode = IDENTITY;
    data.engine = engine;
    data.other = other;
    data.handle = &function;
    CHECK(uqa_cancel_handle_create(engine, &data.cancellation));

    assert(uqa_udf_register_scalar(engine, "c_scalar", options, NULL, &data, destroy_data, &function) == UQA_ERROR_INVALID_ARGUMENT);
    assert(function == NULL && data.destroyed == 0);
    CHECK(uqa_udf_options_set_volatility(options, UQA_VOLATILITY_IMMUTABLE));
    CHECK(uqa_udf_options_set_may_mutate_engine(options, 1));
    assert(uqa_udf_register_scalar(engine, "c_scalar", options, scalar, &data, destroy_data, &function) == UQA_ERROR_SQL);
    assert(function == NULL && data.destroyed == 0);
    CHECK(uqa_udf_options_set_may_mutate_engine(options, 0));
    CHECK(uqa_udf_options_set_volatility(options, UQA_VOLATILITY_VOLATILE));
    assert(uqa_udf_register_scalar(engine, " ", options, scalar, &data, destroy_data, &function) == UQA_ERROR_SQL);
    assert(function == NULL && data.destroyed == 0);
    CHECK(uqa_udf_register_scalar(engine, "c_scalar", options, scalar, &data, destroy_data, &function));
    CHECK(uqa_udf_options_destroy(&options));
    uqa_udf original = function;
    assert(uqa_udf_register_scalar(engine, "c_scalar", NULL, scalar, &data, NULL, &function) == UQA_ERROR_INVALID_ARGUMENT);
    assert(function == original);

    CHECK(uqa_engine_execute(engine, "SELECT c_scalar(42), c_scalar(NULL), c_scalar('abc'::text)", result));
    assert(udf_result_int(result, 0, 0) == 42);
    uqa_value_view value = NULL;
    uint8_t is_null = 0;
    CHECK(uqa_result_get_value(result, 0, 1, &value));
    CHECK(uqa_value_get_is_null(value, &is_null));
    assert(is_null == 1);
    CHECK(uqa_engine_execute(engine, "SELECT c_scalar('550e8400-e29b-41d4-a716-446655440000'::uuid), c_scalar(12.30::numeric)", result));
    uqa_string_view text = {NULL, 0};
    CHECK(uqa_result_get_value(result, 0, 0, &value));
    CHECK(uqa_value_get_uuid(value, &text));
    assert(udf_equal(text, "550e8400-e29b-41d4-a716-446655440000"));
    CHECK(uqa_result_get_value(result, 0, 1, &value));
    CHECK(uqa_value_get_numeric(value, &text));
    assert(udf_equal(text, "12.30"));
    CHECK(uqa_engine_execute(engine, "WITH x AS (SELECT c_scalar(17) AS v) SELECT v FROM x", result));
    assert(udf_result_int(result, 0, 0) == 17);
    data.mode = NULL_RESULT;
    CHECK(uqa_engine_execute(engine, "SELECT c_scalar(1)", result));
    CHECK(uqa_result_get_value(result, 0, 0, &value));
    CHECK(uqa_value_get_is_null(value, &is_null));
    assert(is_null == 1);

    data.mode = MISSING;
    assert(uqa_engine_execute(engine, "SELECT c_scalar(1)", result) == UQA_ERROR_SQL);
    udf_failure("function callback did not assign a result", UQA_ERROR_STATE);
    data.mode = FAIL;
    assert(uqa_engine_execute(engine, "SELECT c_scalar(1)", result) == UQA_ERROR_SQL);
    udf_failure("scalar original failure", UQA_ERROR_SQL);
    data.mode = NEGATIVE;
    assert(uqa_engine_execute(engine, "SELECT c_scalar(1)", result) == UQA_ERROR_SQL);
    udf_failure("C callback failed without a diagnostic", UQA_ERROR_IO);
    data.mode = POSITIVE;
    assert(uqa_engine_execute(engine, "SELECT c_scalar(1)", result) == UQA_ERROR_SQL);
    udf_failure("unexpected positive function callback status", UQA_ERROR_INVALID_ARGUMENT);

    data.mode = CANCEL_RESULT;
    assert(uqa_engine_execute(engine, "SELECT c_scalar(1)", result) == UQA_ERROR_SQL);
    udf_sqlstate("57014");
    CHECK(uqa_engine_reset_cancellation(engine));
    data.mode = CANCEL_ASSIGNMENT;
    assert(uqa_engine_execute(engine, "SELECT c_scalar(1)", result) == UQA_ERROR_SQL);
    udf_sqlstate("57014");
    CHECK(uqa_engine_reset_cancellation(engine));

    /* Planning may evaluate an immutable call; its owned diagnostic must
     * survive whichever native preparation/execution boundary reports it. */
    data.mode = FAIL;
    uqa_udf immutable = NULL;
    uqa_statement failing = NULL;
    CHECK(uqa_udf_options_create(&options));
    CHECK(uqa_udf_options_set_volatility(options, UQA_VOLATILITY_IMMUTABLE));
    CHECK(uqa_udf_options_set_may_mutate_engine(options, 0));
    CHECK(uqa_udf_register_scalar(engine, "c_immutable", options, scalar, &data, NULL, &immutable));
    CHECK(uqa_udf_options_destroy(&options));
    uqa_status planned = uqa_statement_prepare(engine, "SELECT c_immutable(1)", &failing);
    if (planned == UQA_OK) planned = uqa_statement_execute(failing, result);
    assert(planned == UQA_ERROR_SQL);
    udf_failure("scalar original failure", UQA_ERROR_SQL);
    CHECK(uqa_statement_finalize(&failing));
    CHECK(uqa_udf_unregister(&immutable));

    data.mode = REENTRY;
    CHECK(uqa_engine_execute(engine, "SELECT c_scalar(1)", result));
    assert(udf_result_int(result, 0, 0) == 81 && function == original);
    CHECK(uqa_result_destroy(&result));
    CHECK(uqa_result_create(&result));
    data.mode = BLOCK;
    assert(pthread_mutex_init(&data.mutex, NULL) == 0);
    assert(pthread_cond_init(&data.condition, NULL) == 0);
    pthread_t worker;
    assert(pthread_create(&worker, NULL, execute_blocked, &data) == 0);
    assert(pthread_mutex_lock(&data.mutex) == 0);
    while (!data.entered) assert(pthread_cond_wait(&data.condition, &data.mutex) == 0);
    assert(pthread_mutex_unlock(&data.mutex) == 0);
    assert(uqa_udf_unregister(&function) == UQA_ERROR_BUSY);
    assert(function == original && data.destroyed == 0);
    assert(pthread_mutex_lock(&data.mutex) == 0);
    data.released = 1;
    assert(pthread_cond_broadcast(&data.condition) == 0);
    assert(pthread_mutex_unlock(&data.mutex) == 0);
    assert(pthread_join(worker, NULL) == 0);
    assert(pthread_cond_destroy(&data.condition) == 0);
    assert(pthread_mutex_destroy(&data.mutex) == 0);

    /* Replacement and old-handle retirement have distinct identities. */
    struct data replacement;
    memset(&replacement, 0, sizeof(replacement));
    replacement.mode = CONSTANT;
    replacement.number = 73;
    uqa_udf newer = NULL;
    uqa_statement prepared = NULL;
    CHECK(uqa_statement_prepare(engine, "SELECT c_scalar(1)", &prepared));
    CHECK(uqa_udf_register_scalar(engine, "c_scalar", NULL, scalar, &replacement, destroy_data, &newer));
    data.fail_destroy = 1;
    assert(uqa_udf_unregister(&function) == UQA_ERROR_IO);
    assert(function == NULL && data.destroyed == 1);
    CHECK(uqa_cancel_handle_destroy(&data.cancellation));
    CHECK(uqa_udf_unregister(&function));
    CHECK(uqa_udf_unregister(NULL));
    CHECK(uqa_engine_execute(engine, "SELECT c_scalar(1)", result));
    assert(udf_result_int(result, 0, 0) == 73);
    CHECK(uqa_statement_execute(prepared, result));
    assert(udf_result_int(result, 0, 0) == 73);
    CHECK(uqa_statement_finalize(&prepared));

    /* The creator can close while a sibling and/or registration handle live. */
    uqa_engine sibling = NULL;
    if (test_database_flags() != UQA_OPEN_MEMORY) CHECK(uqa_engine_open_session(engine, &sibling));
    CHECK(uqa_result_destroy(&result));
    CHECK(uqa_engine_close(&engine));
    assert(replacement.destroyed == 0);
    if (sibling != NULL) {
        CHECK(uqa_result_create(&result));
        CHECK(uqa_engine_execute(sibling, "SELECT c_scalar(1)", result));
        assert(udf_result_int(result, 0, 0) == 73);
        CHECK(uqa_result_destroy(&result));
        CHECK(uqa_engine_close(&sibling));
    }
    replacement.fail_destroy = 1;
    assert(uqa_udf_unregister(&newer) == UQA_ERROR_IO);
    assert(newer == NULL && replacement.destroyed == 1);
    CHECK(uqa_engine_close(&other));

    uqa_config config = NULL;
    CHECK(uqa_config_create(&config));
    CHECK(uqa_config_set_ffi_memory_limit(config, 1));
    CHECK(test_database_open(config, &engine));
    memset(&data, 0, sizeof(data));
    assert(uqa_udf_register_scalar(engine, "c_scalar", NULL, scalar, &data, destroy_data, &function) == UQA_ERROR_LIMIT);
    assert(function == NULL && data.destroyed == 0);
    CHECK(uqa_engine_close(&engine));
    CHECK(uqa_config_set_ffi_memory_limit(config, 16384));
    CHECK(test_database_open(config, &engine));
    CHECK(uqa_config_destroy(&config));
    data.mode = LIMIT_RESULT;
    CHECK(uqa_udf_register_scalar(engine, "c_scalar", NULL, scalar, &data, destroy_data, &function));
    CHECK(uqa_result_create(&result));
    CHECK(uqa_engine_execute(engine, "SELECT c_scalar(1)", result));
    assert(udf_result_int(result, 0, 0) == 51);
    CHECK(uqa_udf_unregister(&function));
    assert(data.destroyed == 1);
    CHECK(uqa_result_destroy(&result));
    CHECK(uqa_engine_close(&engine));
    return 0;
}
