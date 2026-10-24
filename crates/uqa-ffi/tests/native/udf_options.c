/*
 * Unified Query Algebra
 *
 * Copyright (c) 2023-2026 Cognica, Inc.
 */

#include <uqa/uqa_udf.h>
#include "test_database.h"
#include <assert.h>
#include <stdio.h>
#include <stdlib.h>

static void checked(uqa_status status, int line) {
    if (status != UQA_OK) {
        fprintf(stderr, "line %d: status %d\n", line, (int)status);
        abort();
    }
}
#define CHECK(call) checked((call), __LINE__)

static void properties(uqa_udf_options options, uint32_t volatility, uint8_t may_mutate) {
    uint32_t actual_volatility = UINT32_MAX;
    uint8_t actual_mutation = UINT8_MAX;
    CHECK(uqa_udf_options_get_volatility(options, &actual_volatility));
    CHECK(uqa_udf_options_get_may_mutate_engine(options, &actual_mutation));
    assert(actual_volatility == volatility);
    assert(actual_mutation == may_mutate);
}

int main(void) {
    uqa_engine engine = NULL;
    uqa_result result = NULL;
    uqa_udf_options options = NULL, independent = NULL;
    uint32_t volatility = UINT32_MAX, state = UINT32_MAX;
    uint8_t mutation = UINT8_MAX;
    assert(UQA_VOLATILITY_IMMUTABLE == 0);
    assert(UQA_VOLATILITY_STABLE == 1);
    assert(UQA_VOLATILITY_VOLATILE == 2);
    CHECK(test_database_open(NULL, &engine));
    CHECK(uqa_result_create(&result));
    CHECK(uqa_engine_execute(engine, "BEGIN", result));
    CHECK(uqa_udf_options_create(&options));
    CHECK(uqa_udf_options_create(&independent));
    properties(options, UQA_VOLATILITY_VOLATILE, 1);

    /* Properties are independent until registration validates their combination. */
    const uint32_t codes[] = {
        UQA_VOLATILITY_IMMUTABLE, UQA_VOLATILITY_STABLE, UQA_VOLATILITY_VOLATILE
    };
    for (size_t i = 0; i < sizeof(codes) / sizeof(codes[0]); ++i) {
        for (uint8_t flag = 0; flag <= 1; ++flag) {
            CHECK(uqa_udf_options_set_volatility(options, codes[i]));
            CHECK(uqa_udf_options_set_may_mutate_engine(options, flag));
            properties(options, codes[i], flag);
            assert(uqa_udf_options_set_volatility(options, UINT32_MAX) == UQA_ERROR_INVALID_ARGUMENT);
            assert(uqa_udf_options_set_may_mutate_engine(options, 2) == UQA_ERROR_INVALID_ARGUMENT);
            assert(uqa_udf_options_set_may_mutate_engine(options, UINT8_MAX) == UQA_ERROR_INVALID_ARGUMENT);
            properties(options, codes[i], flag);
        }
    }
    properties(independent, UQA_VOLATILITY_VOLATILE, 1);
    uqa_udf_options original = options;
    assert(uqa_udf_options_create(&options) == UQA_ERROR_INVALID_ARGUMENT);
    assert(options == original);
    assert(uqa_udf_options_create(NULL) == UQA_ERROR_INVALID_ARGUMENT);
    assert(uqa_udf_options_get_volatility(NULL, &volatility) == UQA_ERROR_INVALID_ARGUMENT);
    assert(volatility == 0);
    assert(uqa_udf_options_get_may_mutate_engine(NULL, &mutation) == UQA_ERROR_INVALID_ARGUMENT);
    assert(mutation == 0);
    assert(uqa_udf_options_get_volatility(options, NULL) == UQA_ERROR_INVALID_ARGUMENT);
    assert(uqa_udf_options_get_may_mutate_engine(options, NULL) == UQA_ERROR_INVALID_ARGUMENT);
    assert(uqa_udf_options_set_volatility(NULL, UQA_VOLATILITY_STABLE) == UQA_ERROR_INVALID_ARGUMENT);
    assert(uqa_udf_options_set_may_mutate_engine(NULL, 0) == UQA_ERROR_INVALID_ARGUMENT);
    properties(options, UQA_VOLATILITY_VOLATILE, 1);
    CHECK(uqa_transaction_get_state(engine, &state));
    assert(state == UQA_TRANSACTION_ACTIVE);
    CHECK(uqa_engine_execute(engine, "ROLLBACK", result));
    CHECK(uqa_result_destroy(&result));
    CHECK(uqa_engine_close(&engine));

    /* Options own no Engine lease and survive its close. */
    CHECK(uqa_udf_options_set_volatility(options, UQA_VOLATILITY_STABLE));
    CHECK(uqa_udf_options_set_may_mutate_engine(options, 0));
    properties(options, UQA_VOLATILITY_STABLE, 0);
    CHECK(uqa_udf_options_destroy(&options));
    CHECK(uqa_udf_options_destroy(&independent));
    assert(options == NULL && independent == NULL);
    CHECK(uqa_udf_options_destroy(&options));
    CHECK(uqa_udf_options_destroy(NULL));
    return 0;
}
