/*
 * Unified Query Algebra
 *
 * Copyright (c) 2023-2026 Cognica, Inc.
 */

#include "test_database.h"
#include <stddef.h>

#ifdef __cplusplus
#define STATIC_ASSERT static_assert
#else
#define STATIC_ASSERT _Static_assert
#endif
STATIC_ASSERT(sizeof(uqa_date) == 8, "date layout");
STATIC_ASSERT(sizeof(uqa_timestamp) == 16, "timestamp layout");
STATIC_ASSERT(sizeof(uqa_timetz) == 16, "timetz layout");
STATIC_ASSERT(sizeof(uqa_interval) == 24, "interval layout");
STATIC_ASSERT(offsetof(uqa_timestamp, kind) == 8, "timestamp kind offset");
STATIC_ASSERT(offsetof(uqa_interval, kind) == 16, "interval kind offset");

/* SQL results are independently captured in the Engine temporal_carriers.json
 * oracle and Core temporal production fixtures against PostgreSQL 18.4. */
static void checked(uqa_status status, int line) {
    if (status != UQA_OK) {
        uqa_error error = NULL;
        uqa_string_view message = {NULL, 0};
        if (uqa_error_get_last(&error) == UQA_OK) (void)uqa_error_get_message(error, &message);
        fprintf(stderr, "line %d: %d: %.*s\n", line, (int)status,
            (int)message.size, message.data == NULL ? "" : message.data);
        abort();
    }
}
#define CHECK(call) checked((call), __LINE__)

static uqa_value_view cell(uqa_result result, uint32_t column) {
    uqa_value_view value = NULL;
    CHECK(uqa_result_get_value(result, 0, column, &value));
    return value;
}

static void text(uqa_value_view value, const char *expected) {
    uqa_string_view actual = {NULL, 0};
    CHECK(uqa_value_get_text(value, &actual));
    assert(actual.size == strlen(expected));
    assert(memcmp(actual.data, expected, actual.size) == 0);
}

static void parameter(uqa_engine engine, uqa_result result, uqa_value_view value,
    const char *expected, const char *type_name) {
    const uqa_value_view params[] = {value};
    CHECK(uqa_engine_execute_params(engine, "SELECT $1::text, pg_typeof($1)::text", params, 1, result));
    text(cell(result, 0), expected);
    text(cell(result, 1), type_name);
}

static void dates(uqa_engine engine, uqa_result result) {
    static const uqa_date inputs[] = {
        {-2451545, UQA_TEMPORAL_FINITE}, {0, UQA_TEMPORAL_FINITE},
        {2145031948, UQA_TEMPORAL_FINITE}, {123, UQA_TEMPORAL_NEGATIVE_INFINITY},
        {-123, UQA_TEMPORAL_POSITIVE_INFINITY}
    };
    static const char *outputs[] = {
        "4714-11-24 BC", "2000-01-01", "5874897-12-31", "-infinity", "infinity"
    };
    for (size_t i = 0; i < sizeof(inputs) / sizeof(inputs[0]); ++i) {
        uqa_value value = NULL, copy = NULL;
        uqa_date actual = {0, 0};
        CHECK(uqa_value_create_date(inputs[i], &value));
        CHECK(uqa_value_create_copy(value, &copy));
        CHECK(uqa_value_destroy(&value));
        CHECK(uqa_value_get_date(copy, &actual));
        assert(actual.kind == inputs[i].kind);
        assert(actual.days == (actual.kind == UQA_TEMPORAL_FINITE ? inputs[i].days : 0));
        parameter(engine, result, copy, outputs[i], "date");
        CHECK(uqa_value_destroy(&copy));
    }
    const int32_t outside[] = {INT32_MIN, -2451546, 2145031949, INT32_MAX};
    for (size_t i = 0; i < sizeof(outside) / sizeof(outside[0]); ++i) {
        uqa_value value = NULL;
        const uqa_date input = {outside[i], UQA_TEMPORAL_FINITE};
        assert(uqa_value_create_date(input, &value) == UQA_ERROR_RANGE && value == NULL);
    }
    uqa_value value = NULL;
    const uqa_date invalid = {0, UINT32_MAX};
    assert(uqa_value_create_date(invalid, &value) == UQA_ERROR_INVALID_ARGUMENT && value == NULL);
}

static void timestamps(uqa_engine engine, uqa_result result) {
    static const uqa_timestamp inputs[] = {
        {-INT64_C(211813488000000000), UQA_TEMPORAL_FINITE, 0},
        {-1, UQA_TEMPORAL_FINITE, 0}, {0, UQA_TEMPORAL_FINITE, 0},
        {INT64_C(9223371331199999999), UQA_TEMPORAL_FINITE, 0},
        {123, UQA_TEMPORAL_NEGATIVE_INFINITY, 0},
        {123, UQA_TEMPORAL_POSITIVE_INFINITY, 0}
    };
    static const char *outputs[][2] = {
        {"4714-11-24 00:00:00 BC", "4714-11-24 00:00:00+00 BC"},
        {"1999-12-31 23:59:59.999999", "1999-12-31 23:59:59.999999+00"},
        {"2000-01-01 00:00:00", "2000-01-01 00:00:00+00"},
        {"294276-12-31 23:59:59.999999", "294276-12-31 23:59:59.999999+00"},
        {"-infinity", "-infinity"}, {"infinity", "infinity"}
    };
    for (int zoned = 0; zoned < 2; ++zoned) {
        for (size_t i = 0; i < sizeof(inputs) / sizeof(inputs[0]); ++i) {
            uqa_value value = NULL;
            uqa_timestamp actual = {0, 0, 0};
            CHECK(zoned ? uqa_value_create_timestamptz(inputs[i], &value) : uqa_value_create_timestamp(inputs[i], &value));
            CHECK(zoned ? uqa_value_get_timestamptz(value, &actual) : uqa_value_get_timestamp(value, &actual));
            assert(actual.kind == inputs[i].kind && actual.reserved == 0);
            assert(actual.micros == (actual.kind == UQA_TEMPORAL_FINITE ? inputs[i].micros : 0));
            parameter(engine, result, value, outputs[i][zoned], zoned ? "timestamp with time zone" : "timestamp without time zone");
            memset(&actual, 0xa5, sizeof(actual));
            assert((zoned ? uqa_value_get_timestamp(value, &actual) : uqa_value_get_timestamptz(value, &actual)) == UQA_ERROR_TYPE);
            assert(actual.micros == 0 && actual.kind == 0 && actual.reserved == 0);
            CHECK(uqa_value_destroy(&value));
        }
        const int64_t outside[] = {INT64_MIN, -INT64_C(211813488000000001), INT64_C(9223371331200000000), INT64_MAX};
        for (size_t i = 0; i < sizeof(outside) / sizeof(outside[0]); ++i) {
            uqa_value value = NULL;
            const uqa_timestamp input = {outside[i], UQA_TEMPORAL_FINITE, 0};
            assert((zoned ? uqa_value_create_timestamptz(input, &value) : uqa_value_create_timestamp(input, &value)) == UQA_ERROR_RANGE);
            assert(value == NULL);
        }
    }
    uqa_value value = NULL;
    const uqa_timestamp invalid_kind = {0, UINT32_MAX, 0}, reserved = {0, UQA_TEMPORAL_FINITE, 1};
    assert(uqa_value_create_timestamp(invalid_kind, &value) == UQA_ERROR_INVALID_ARGUMENT && value == NULL);
    assert(uqa_value_create_timestamptz(reserved, &value) == UQA_ERROR_INVALID_ARGUMENT && value == NULL);
}

static void clocks(uqa_engine engine, uqa_result result) {
    const int64_t times[] = {0, 1, INT64_C(86400000000)};
    const char *outputs[] = {"00:00:00", "00:00:00.000001", "24:00:00"};
    for (size_t i = 0; i < sizeof(times) / sizeof(times[0]); ++i) {
        uqa_value value = NULL;
        int64_t actual = -1;
        CHECK(uqa_value_create_time(times[i], &value));
        CHECK(uqa_value_get_time(value, &actual));
        assert(actual == times[i]);
        parameter(engine, result, value, outputs[i], "time without time zone");
        CHECK(uqa_value_destroy(&value));
    }
    const uqa_timetz inputs[] = {{0, 1, 0}, {INT64_C(86400000000), -57599, 0}, {INT64_C(45296123456), 57599, 0}};
    const char *zoned[] = {"00:00:00+00:00:01", "24:00:00-15:59:59", "12:34:56.123456+15:59:59"};
    for (size_t i = 0; i < sizeof(inputs) / sizeof(inputs[0]); ++i) {
        uqa_value value = NULL;
        uqa_timetz actual = {0, 0, 0};
        CHECK(uqa_value_create_timetz(inputs[i], &value));
        CHECK(uqa_value_get_timetz(value, &actual));
        assert(actual.micros == inputs[i].micros && actual.offset_seconds == inputs[i].offset_seconds && actual.reserved == 0);
        parameter(engine, result, value, zoned[i], "time with time zone");
        CHECK(uqa_value_destroy(&value));
    }
    uqa_value value = NULL;
    assert(uqa_value_create_time(-1, &value) == UQA_ERROR_RANGE && value == NULL);
    assert(uqa_value_create_time(INT64_C(86400000001), &value) == UQA_ERROR_RANGE && value == NULL);
    const uqa_timetz bad_zone = {0, 57600, 0}, reserved = {0, 0, 1};
    assert(uqa_value_create_timetz(bad_zone, &value) == UQA_ERROR_RANGE && value == NULL);
    assert(uqa_value_create_timetz(reserved, &value) == UQA_ERROR_INVALID_ARGUMENT && value == NULL);
}

static void intervals(uqa_engine engine, uqa_result result) {
    const uqa_interval inputs[] = {
        {3, -2, 1, UQA_TEMPORAL_FINITE, 0},
        {INT64_MAX - 1, INT32_MAX, INT32_MAX, UQA_TEMPORAL_FINITE, 0},
        {7, 8, 9, UQA_TEMPORAL_POSITIVE_INFINITY, 0},
        {7, 8, 9, UQA_TEMPORAL_NEGATIVE_INFINITY, 0}
    };
    for (size_t i = 0; i < sizeof(inputs) / sizeof(inputs[0]); ++i) {
        uqa_value value = NULL;
        uqa_interval actual = {0, 0, 0, 0, 0};
        CHECK(uqa_value_create_interval(inputs[i], &value));
        CHECK(uqa_value_get_interval(value, &actual));
        assert(actual.kind == inputs[i].kind && actual.reserved == 0);
        if (actual.kind == UQA_TEMPORAL_FINITE) {
            assert(actual.micros == inputs[i].micros && actual.days == inputs[i].days && actual.months == inputs[i].months);
        } else {
            assert(actual.micros == 0 && actual.days == 0 && actual.months == 0);
            parameter(engine, result, value, actual.kind == UQA_TEMPORAL_POSITIVE_INFINITY ? "infinity" : "-infinity", "interval");
        }
        CHECK(uqa_value_destroy(&value));
    }
    uqa_value value = NULL;
    const uqa_interval sentinel = {INT64_MAX, INT32_MAX, INT32_MAX, UQA_TEMPORAL_FINITE, 0};
    assert(uqa_value_create_interval(sentinel, &value) == UQA_ERROR_RANGE && value == NULL);
}

static void nulls_and_domains(uqa_engine engine, uqa_result result) {
    uqa_value value = NULL;
    CHECK(uqa_value_create_null(&value));
    uqa_date date; uqa_timestamp stamp; uqa_timetz zone; uqa_interval interval; int64_t time = -1;
    memset(&date, 0xa5, sizeof(date)); memset(&stamp, 0xa5, sizeof(stamp));
    memset(&zone, 0xa5, sizeof(zone)); memset(&interval, 0xa5, sizeof(interval));
    assert(uqa_value_get_date(value, &date) == UQA_ERROR_NULL && date.days == 0 && date.kind == 0);
    assert(uqa_value_get_time(value, &time) == UQA_ERROR_NULL && time == 0);
    assert(uqa_value_get_timetz(value, &zone) == UQA_ERROR_NULL && zone.micros == 0 && zone.offset_seconds == 0 && zone.reserved == 0);
    assert(uqa_value_get_timestamp(value, &stamp) == UQA_ERROR_NULL && stamp.micros == 0 && stamp.kind == 0 && stamp.reserved == 0);
    memset(&stamp, 0xa5, sizeof(stamp));
    assert(uqa_value_get_timestamptz(value, &stamp) == UQA_ERROR_NULL && stamp.micros == 0 && stamp.kind == 0 && stamp.reserved == 0);
    assert(uqa_value_get_interval(value, &interval) == UQA_ERROR_NULL && interval.micros == 0 && interval.days == 0 && interval.months == 0 && interval.kind == 0 && interval.reserved == 0);
    CHECK(uqa_value_destroy(&value));
    CHECK(uqa_engine_execute(engine, "CREATE DOMAIN precise_stamp AS timestamp(0)", result));
    CHECK(uqa_value_create_from_text(engine, "precise_stamp", "2000-01-01 00:00:00.5", &value));
    CHECK(uqa_value_get_timestamp(value, &stamp));
    assert(stamp.micros == INT64_C(1000000) && stamp.kind == UQA_TEMPORAL_FINITE);
    CHECK(uqa_value_destroy(&value));
    CHECK(uqa_engine_execute(engine, "SELECT CAST(timestamp '294276-12-31 23:59:59.999999' AS timestamp(0))", result));
    CHECK(uqa_value_get_timestamp(cell(result, 0), &stamp));
    assert(stamp.micros == INT64_C(9223371331200000000) && stamp.kind == UQA_TEMPORAL_FINITE);
}

static void verify_row(uqa_result result) {
    uqa_date date = {0, 0}; uqa_timetz zone = {0, 0, 0}; uqa_timestamp stamp = {0, 0, 0};
    uqa_interval interval = {0, 0, 0, 0, 0}; int64_t time = 0;
    CHECK(uqa_value_get_date(cell(result, 0), &date));
    CHECK(uqa_value_get_time(cell(result, 1), &time));
    CHECK(uqa_value_get_timetz(cell(result, 2), &zone));
    CHECK(uqa_value_get_timestamp(cell(result, 3), &stamp));
    assert(date.days == 2145031948 && date.kind == UQA_TEMPORAL_FINITE && time == INT64_C(86400000000));
    assert(zone.micros == time && zone.offset_seconds == -57599 && stamp.micros == INT64_C(9223371331199999999));
    CHECK(uqa_value_get_timestamptz(cell(result, 4), &stamp));
    CHECK(uqa_value_get_interval(cell(result, 5), &interval));
    assert(stamp.kind == UQA_TEMPORAL_NEGATIVE_INFINITY && interval.kind == UQA_TEMPORAL_POSITIVE_INFINITY);
}

static void stored(uqa_engine engine, uqa_result result) {
    CHECK(uqa_engine_execute(engine, "SELECT d,t,z,ts,tz,i FROM c_temporal", result));
    verify_row(result);
}

int main(void) {
    const char *directory = getenv("UQA_TEST_DATABASE_DIR");
    char path[4096];
    assert(directory != NULL);
    int length = snprintf(path, sizeof(path), "%s/temporal.db", directory);
    assert(length > 0 && (size_t)length < sizeof(path));
    uqa_engine engine = NULL;
    uqa_result result = NULL;
    CHECK(test_database_open_path(path, NULL, &engine));
    CHECK(uqa_result_create(&result));
    dates(engine, result); timestamps(engine, result); clocks(engine, result); intervals(engine, result);
    nulls_and_domains(engine, result);
    CHECK(uqa_engine_execute(engine, "CREATE TABLE c_temporal(d date,t time,z timetz,ts timestamp,tz timestamptz,i interval)", result));
    uqa_value values[6] = {NULL, NULL, NULL, NULL, NULL, NULL};
    const uqa_date date = {2145031948, UQA_TEMPORAL_FINITE};
    const uqa_timetz zone = {INT64_C(86400000000), -57599, 0};
    const uqa_timestamp stamp = {INT64_C(9223371331199999999), UQA_TEMPORAL_FINITE, 0};
    const uqa_timestamp negative = {0, UQA_TEMPORAL_NEGATIVE_INFINITY, 0};
    const uqa_interval positive = {0, 0, 0, UQA_TEMPORAL_POSITIVE_INFINITY, 0};
    CHECK(uqa_value_create_date(date, &values[0]));
    CHECK(uqa_value_create_time(INT64_C(86400000000), &values[1]));
    CHECK(uqa_value_create_timetz(zone, &values[2]));
    CHECK(uqa_value_create_timestamp(stamp, &values[3]));
    CHECK(uqa_value_create_timestamptz(negative, &values[4]));
    CHECK(uqa_value_create_interval(positive, &values[5]));
    const uqa_value_view params[] = {values[0], values[1], values[2], values[3], values[4], values[5]};
    CHECK(uqa_engine_execute_params(engine, "INSERT INTO c_temporal VALUES ($1,$2,$3,$4,$5,$6)", params, 6, result));
    uqa_statement statement = NULL;
    uqa_cursor cursor = NULL;
    const char *types[] = {"date", "time", "timetz", "timestamp", "timestamptz", "interval"};
    CHECK(uqa_statement_prepare_typed(engine, "SELECT $1,$2,$3,$4,$5,$6", types, 6, &statement));
    for (size_t i = 0; i < 6; ++i) CHECK(uqa_statement_bind(statement, (uint32_t)i + 1, values[i]));
    CHECK(uqa_cursor_open_params(engine, "SELECT $1,$2,$3,$4,$5,$6", params, 6, &cursor));
    for (size_t i = 0; i < 6; ++i) CHECK(uqa_value_destroy(&values[i]));
    CHECK(uqa_statement_execute(statement, result));
    verify_row(result);
    CHECK(uqa_statement_finalize(&statement));
    CHECK(uqa_cursor_fetch(cursor, 1, result));
    verify_row(result);
    assert(uqa_cursor_fetch(cursor, 1, result) == UQA_DONE);
    CHECK(uqa_cursor_close(&cursor));
    stored(engine, result);
    CHECK(uqa_result_destroy(&result));
    CHECK(uqa_engine_close(&engine));
    if (test_database_flags() != UQA_OPEN_MEMORY) {
        CHECK(test_database_open_path(path, NULL, &engine));
        CHECK(uqa_result_create(&result));
        stored(engine, result);
        CHECK(uqa_result_destroy(&result));
        CHECK(uqa_engine_close(&engine));
    }
    return 0;
}
