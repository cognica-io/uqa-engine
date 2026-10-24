/*
 * Unified Query Algebra
 *
 * Copyright (c) 2023-2026 Cognica, Inc.
 */

#include "test_udf.h"

struct data {
    int attempts, created, observed, finished, destroyed, released;
    int fail_create_at, fail_observe, fail_finish, fail_destroy, cancel, null_state;
    int fill_memory, retained;
    uqa_value copies[256];
    uqa_cancel_handle cancellation;
};
struct state { int64_t sum; };

static uqa_status UQA_CALL create_state(void *user_data, void **out_state) {
    struct data *data = (struct data *)user_data;
    ++data->attempts;
    if (data->attempts == data->fail_create_at) return uqa_callback_fail(UQA_ERROR_IO, "aggregate creation failed");
    struct state *state = data->null_state ? NULL : (struct state *)calloc(1, sizeof(*state));
    assert(state != NULL || data->null_state);
    *out_state = state;
    ++data->created;
    return UQA_OK;
}

static uqa_status UQA_CALL observe(void *pointer, uqa_value_view const *arguments,
                                  uint32_t count, void *user_data) {
    struct data *data = (struct data *)user_data;
    struct state *state = (struct state *)pointer;
    assert(count == 1);
    ++data->observed;
    if (data->fill_memory) {
        while (data->retained < 256) {
            uqa_status status = uqa_value_create_copy(arguments[0], &data->copies[data->retained]);
            if (status != UQA_OK) {
                assert(status == UQA_ERROR_LIMIT);
                return uqa_callback_fail(UQA_ERROR_LIMIT, "aggregate retention limit");
            }
            ++data->retained;
        }
        abort();
    }
    if (data->fail_observe) return uqa_callback_fail(UQA_ERROR_TYPE, "aggregate observation failed");
    if (data->cancel) CHECK(uqa_cancel_handle_request(data->cancellation));
    uint8_t is_null = 0;
    CHECK(uqa_value_get_is_null(arguments[0], &is_null));
    if (!is_null && state != NULL) {
        int64_t number = 0;
        CHECK(uqa_value_get_int64(arguments[0], &number));
        state->sum += number;
    }
    return UQA_OK;
}

static uqa_status UQA_CALL finish(uqa_udf_context context, void *pointer, void *user_data) {
    struct data *data = (struct data *)user_data;
    const struct state *state = (const struct state *)pointer;
    ++data->finished;
    if (data->fail_finish) return uqa_udf_context_fail(context, "aggregate finish failed");
    return udf_set_int(context, state == NULL ? 99 : state->sum);
}

static uqa_status UQA_CALL destroy_state(void *pointer, void *user_data) {
    struct data *data = (struct data *)user_data;
    free(pointer);
    while (data->retained > 0) CHECK(uqa_value_destroy(&data->copies[--data->retained]));
    ++data->destroyed;
    assert(data->destroyed <= data->created);
    return data->fail_destroy ? uqa_callback_fail(UQA_ERROR_IO, "aggregate cleanup failed") : UQA_OK;
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
    assert(uqa_udf_register_aggregate(engine, "c_total", options, create_state, observe, finish, NULL, &data, destroy_data, &function) == UQA_ERROR_INVALID_ARGUMENT);
    assert(function == NULL && data.released == 0);
    CHECK(uqa_udf_register_aggregate(engine, "c_total", options, create_state, observe, finish, destroy_state, &data, destroy_data, &function));
    CHECK(uqa_udf_options_destroy(&options));

    CHECK(uqa_engine_execute(engine, "SELECT c_total(n) FROM (VALUES (1), (2), (NULL), (3)) AS v(n)", result));
    assert(udf_result_int(result, 0, 0) == 6 && data.created == data.destroyed);
    int before = data.created;
    CHECK(uqa_engine_execute(engine, "SELECT c_total(n) FROM (SELECT 1 AS n WHERE false) AS v", result));
    assert(udf_result_int(result, 0, 0) == 0 && data.created == before + 1 && data.created == data.destroyed);
    before = data.created;
    CHECK(uqa_engine_execute(engine, "SELECT c_total(n) OVER () FROM (SELECT 1 AS n WHERE false) AS v", result));
    assert(data.created == before);
    CHECK(uqa_engine_execute(engine, "SELECT c_total(DISTINCT n ORDER BY n) FROM (VALUES (2), (1), (2)) AS v(n)", result));
    assert(udf_result_int(result, 0, 0) == 3 && data.created == data.destroyed);
    CHECK(uqa_engine_execute(engine, "SELECT g, c_total(n) FROM (VALUES (1, 1), (1, 2), (2, 3)) AS v(g, n) GROUP BY g ORDER BY g", result));
    assert(udf_result_int(result, 0, 1) == 3 && udf_result_int(result, 1, 1) == 3);
    assert(data.created == data.destroyed);
    CHECK(uqa_engine_execute(engine, "SELECT n, c_total(n) OVER (ORDER BY n ROWS BETWEEN 1 PRECEDING AND 1 PRECEDING) FROM (VALUES (1), (2), (3)) AS v(n) ORDER BY n", result));
    assert(udf_result_int(result, 0, 1) == 0 && udf_result_int(result, 1, 1) == 1 && udf_result_int(result, 2, 1) == 2);
    assert(data.created == data.destroyed);

    data.fail_create_at = data.attempts + 1;
    before = data.created;
    assert(uqa_engine_execute(engine, "SELECT c_total(n) FROM (SELECT 1 AS n WHERE false) AS v", result) == UQA_ERROR_SQL);
    udf_failure("aggregate creation failed", UQA_ERROR_IO);
    assert(data.created == before && data.created == data.destroyed);
    /* Failure of the second aggregate closes the first successful factory. */
    data.fail_create_at = data.attempts + 2;
    assert(uqa_engine_execute(engine, "SELECT c_total(n), c_total(n + 1) FROM (VALUES (1), (2)) AS v(n)", result) == UQA_ERROR_SQL);
    udf_failure("aggregate creation failed", UQA_ERROR_IO);
    assert(data.created == data.destroyed);
    data.fail_create_at = 0;
    data.fail_observe = 1;
    data.fail_destroy = 1;
    assert(uqa_engine_execute(engine, "SELECT c_total(n) FROM (VALUES (1), (2)) AS v(n)", result) == UQA_ERROR_SQL);
    udf_failure("aggregate observation failed", UQA_ERROR_TYPE);
    uqa_error error = NULL;
    uqa_string_view detail = {NULL, 0};
    CHECK(uqa_error_get_last(&error));
    CHECK(uqa_error_get_detail(error, &detail));
    assert(detail.size > 0 && data.created == data.destroyed);
    data.fail_observe = 0;
    data.fail_finish = 1;
    assert(uqa_engine_execute(engine, "SELECT c_total(n) FROM (VALUES (1), (2)) AS v(n)", result) == UQA_ERROR_SQL);
    udf_failure("aggregate finish failed", UQA_ERROR_SQL);
    assert(data.created == data.destroyed);
    data.fail_finish = 0;
    assert(uqa_engine_execute(engine, "SELECT c_total(n) FROM (VALUES (1), (2)) AS v(n)", result) == UQA_ERROR_SQL);
    udf_failure("aggregate cleanup failed", UQA_ERROR_IO);
    assert(data.created == data.destroyed);
    data.fail_destroy = 0;

    CHECK(uqa_engine_execute(engine, "BEGIN; SAVEPOINT before_callback", result));
    data.fail_observe = 1;
    assert(uqa_engine_execute(engine, "SELECT c_total(n) FROM (VALUES (1), (2)) AS v(n)", result) == UQA_ERROR_SQL);
    uint32_t transaction = 0;
    CHECK(uqa_transaction_get_state(engine, &transaction));
    assert(transaction == UQA_TRANSACTION_FAILED);
    CHECK(uqa_savepoint_rollback(engine, "before_callback"));
    data.fail_observe = 0;
    CHECK(uqa_engine_execute(engine, "SELECT c_total(n) FROM (VALUES (1), (2)) AS v(n)", result));
    assert(udf_result_int(result, 0, 0) == 3);
    CHECK(uqa_transaction_rollback(engine));
    data.cancel = 1;
    assert(uqa_engine_execute(engine, "SELECT c_total(n) FROM (VALUES (1), (2)) AS v(n)", result) == UQA_ERROR_SQL);
    udf_sqlstate("57014");
    assert(data.created == data.destroyed);
    CHECK(uqa_engine_reset_cancellation(engine));
    data.cancel = 0;
    data.null_state = 1;
    CHECK(uqa_engine_execute(engine, "SELECT c_total(n) FROM (VALUES (1)) AS v(n)", result));
    assert(udf_result_int(result, 0, 0) == 99 && data.created == data.destroyed);
    CHECK(uqa_udf_unregister(&function));
    assert(function == NULL && data.released == 1);
    CHECK(uqa_cancel_handle_destroy(&data.cancellation));
    CHECK(uqa_result_destroy(&result));
    CHECK(uqa_engine_close(&engine));

    /* Cleanup has its own pre-admitted channel after observe exhausts the
     * invoking session's FFI allowance. It still releases every copied value. */
    memset(&data, 0, sizeof(data));
    data.fill_memory = 1;
    data.fail_destroy = 1;
    uqa_config config = NULL;
    CHECK(uqa_config_create(&config));
    CHECK(uqa_config_set_ffi_memory_limit(config, 16384));
    CHECK(test_database_open(config, &engine));
    CHECK(uqa_config_destroy(&config));
    CHECK(uqa_result_create(&result));
    CHECK(uqa_udf_register_aggregate(engine, "c_total", NULL, create_state, observe, finish, destroy_state, &data, destroy_data, &function));
    assert(uqa_engine_execute(engine, "SELECT c_total(n) FROM (VALUES (1)) AS v(n)", result) == UQA_ERROR_SQL);
    assert(data.observed == 1 && data.created == 1 && data.destroyed == 1 && data.retained == 0);
    CHECK(uqa_error_get_last(&error));
    uqa_error cause = NULL;
    uqa_status status = UQA_OK;
    CHECK(uqa_error_get_cause(error, &cause));
    CHECK(uqa_error_get_status(cause, &status));
    assert(status == UQA_ERROR_LIMIT);
    CHECK(uqa_error_get_detail(error, &detail));
    assert(detail.size > 0);
    CHECK(uqa_udf_unregister(&function));
    assert(data.released == 1);
    CHECK(uqa_result_destroy(&result));
    CHECK(uqa_engine_close(&engine));
    return 0;
}
