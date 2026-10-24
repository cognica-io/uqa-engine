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

/* Expected SQL values and UUID diagnostics were captured independently from
 * PostgreSQL 18.4 in uqa-sql/src/params/packed/postgresql.json. */

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

static int equal(uqa_string_view view, const char *text) {
    size_t size = strlen(text);
    return view.size == size && (size == 0 || memcmp(view.data, text, size) == 0);
}

static uqa_value_view cell(uqa_result result, uint32_t column) {
    uqa_value_view value = NULL;
    CHECK(uqa_result_get_value(result, 0, column, &value));
    return value;
}

static void descriptor(uqa_value_view value, uqa_type_kind kind, uint32_t oid, int32_t modifier) {
    uqa_type type = NULL;
    uqa_type_kind actual_kind = UQA_TYPE_UNKNOWN;
    uint32_t actual_oid = 0;
    int32_t actual_modifier = 0;
    CHECK(uqa_value_get_type(value, &type));
    CHECK(uqa_type_get_kind(type, &actual_kind));
    CHECK(uqa_type_get_oid(type, &actual_oid));
    CHECK(uqa_type_get_modifier(type, &actual_modifier));
    assert(actual_kind == kind && actual_oid == oid && actual_modifier == modifier);
}

static void parameter(uqa_engine engine, uqa_result result, uqa_value_view value,
                      const char *text, const char *type_name) {
    uqa_value_view params[] = {value};
    uqa_string_view actual = {NULL, 0};
    CHECK(uqa_engine_execute_params(engine, "SELECT $1::text, pg_typeof($1)::text", params, 1, result));
    CHECK(uqa_value_get_text(cell(result, 0), &actual));
    assert(equal(actual, text));
    CHECK(uqa_value_get_text(cell(result, 1), &actual));
    assert(equal(actual, type_name));
}

static void uuids(uqa_engine engine, uqa_result result) {
    static const uint8_t inputs[][16] = {
        {0},
        {255,255,255,255,255,255,255,255,255,255,255,255,255,255,255,255},
        {0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15},
        {0x55,0x0e,0x84,0x00,0xe2,0x9b,0x41,0xd4,0xa7,0x16,0x44,0x66,0x55,0x44,0x00,0x00}
    };
    static const char *texts[] = {
        "00000000-0000-0000-0000-000000000000", "ffffffff-ffff-ffff-ffff-ffffffffffff",
        "00010203-0405-0607-0809-0a0b0c0d0e0f", "550e8400-e29b-41d4-a716-446655440000"
    };
    for (size_t i = 0; i < sizeof(inputs) / sizeof(inputs[0]); ++i) {
        uint8_t input[16];
        uqa_value value = NULL, copy = NULL;
        uqa_bytes_view bytes = {NULL, 0};
        uqa_string_view text = {NULL, 0};
        memcpy(input, inputs[i], sizeof(input));
        CHECK(uqa_value_create_uuid_bytes(input, sizeof(input), &value));
        memset(input, 0x5a, sizeof(input));
        descriptor(value, UQA_TYPE_UUID, 2950, -1);
        CHECK(uqa_value_get_uuid_bytes(value, &bytes));
        assert(bytes.size == 16 && memcmp(bytes.data, inputs[i], 16) == 0);
        const uint8_t *first = bytes.data;
        CHECK(uqa_value_get_uuid_bytes(value, &bytes));
        assert(bytes.data == first);
        CHECK(uqa_value_get_uuid(value, &text));
        assert(equal(text, texts[i]));
        assert(uqa_value_get_bytes(value, &bytes) == UQA_ERROR_TYPE && bytes.data == NULL && bytes.size == 0);
        assert(uqa_value_get_text(value, &text) == UQA_ERROR_TYPE && text.data == NULL && text.size == 0);
        CHECK(uqa_value_create_copy(value, &copy));
        CHECK(uqa_value_destroy(&value));
        CHECK(uqa_value_get_uuid_bytes(copy, &bytes));
        assert(bytes.size == 16 && memcmp(bytes.data, inputs[i], 16) == 0);
        parameter(engine, result, copy, texts[i], "uuid");
        CHECK(uqa_value_destroy(&copy));
    }
    const char *spellings[] = {"{550E8400-E29B-41D4-A716-446655440000}",
        "550e8400e29b41d4a716446655440000", "550e-8400-e29b-41d4-a716-4466-5544-0000"};
    for (size_t i = 0; i < sizeof(spellings) / sizeof(spellings[0]); ++i) {
        uqa_value value = NULL;
        CHECK(uqa_value_create_uuid(spellings[i], &value));
        parameter(engine, result, value, texts[3], "uuid");
        CHECK(uqa_value_destroy(&value));
    }
    uqa_value value = NULL;
    char span[32];
    memcpy(span, spellings[1], sizeof(span));
    CHECK(uqa_value_create_uuid_n(span, sizeof(span), &value));
    memset(span, 'x', sizeof(span));
    parameter(engine, result, value, texts[3], "uuid");
    CHECK(uqa_value_destroy(&value));
    const char *invalid[] = {" 550e8400-e29b-41d4-a716-446655440000", "not-a-uuid"};
    for (size_t i = 0; i < sizeof(invalid) / sizeof(invalid[0]); ++i) {
        uqa_error error = NULL;
        uqa_string_view state = {NULL, 0}, message = {NULL, 0};
        char expected[128];
        assert(uqa_value_create_uuid(invalid[i], &value) == UQA_ERROR_SQL && value == NULL);
        CHECK(uqa_error_get_last(&error));
        CHECK(uqa_error_get_sqlstate(error, &state));
        assert(equal(state, "22P02"));
        CHECK(uqa_error_get_message(error, &message));
        int length = snprintf(expected, sizeof(expected), "invalid input syntax for type uuid: \"%s\"", invalid[i]);
        assert(length > 0 && (size_t)length < sizeof(expected) && equal(message, expected));
    }
    assert(uqa_value_create_uuid_bytes(inputs[0], 15, &value) == UQA_ERROR_INVALID_ARGUMENT && value == NULL);
    assert(uqa_value_create_uuid_bytes(NULL, 16, &value) == UQA_ERROR_INVALID_ARGUMENT && value == NULL);
    CHECK(uqa_engine_execute(engine, "SELECT NULL::uuid, '550e8400-e29b-41d4-a716-446655440000'::text", result));
    uqa_bytes_view bytes = {NULL, 99};
    assert(uqa_value_get_uuid_bytes(cell(result, 0), &bytes) == UQA_ERROR_NULL && bytes.data == NULL && bytes.size == 0);
    assert(uqa_value_get_uuid_bytes(cell(result, 1), &bytes) == UQA_ERROR_TYPE && bytes.data == NULL && bytes.size == 0);
}

static void bits(uqa_engine engine, uqa_result result) {
    static const struct {
        uint32_t length;
        uint8_t bytes[4];
        const char *text;
    } cases[] = {
        {0,{0},""}, {1,{0xff},"1"}, {3,{0xa7},"101"}, {7,{0x81},"1000000"},
        {8,{0x81},"10000001"}, {9,{0,0xff},"000000001"},
        {15,{0x12,0x35},"000100100011010"}, {16,{0x12,0x35},"0001001000110101"},
        {17,{0x12,0x34,0xff},"00010010001101001"},
        {32,{0,1,0x80,0xff},"00000000000000011000000011111111"}
    };
    for (int varying = 0; varying < 2; ++varying) {
        for (size_t i = 0; i < sizeof(cases) / sizeof(cases[0]); ++i) {
            uqa_value value = NULL, copy = NULL;
            uqa_bits_view view = {NULL, 0, 0};
            uint8_t input[4];
            size_t size = (cases[i].length + 7) / 8;
            memcpy(input, cases[i].bytes, sizeof(input));
            const uint8_t *data = size == 0 ? NULL : input;
            CHECK(varying ? uqa_value_create_varbit(data, size, cases[i].length, &value)
                          : uqa_value_create_bit(data, size, cases[i].length, &value));
            memset(input, 0x5a, sizeof(input));
            descriptor(value, varying ? UQA_TYPE_VARBIT : UQA_TYPE_BIT, varying ? 1562 : 1560, -1);
            CHECK(varying ? uqa_value_get_varbit(value, &view) : uqa_value_get_bit(value, &view));
            assert(view.bit_length == cases[i].length && view.size == size);
            for (uint32_t j = 0; j < view.bit_length; ++j)
                assert(((view.data[j / 8] >> (7 - j % 8)) & 1) == (unsigned)(cases[i].text[j] - '0'));
            if (view.bit_length % 8 != 0)
                assert((view.data[size - 1] & ((1u << (8 - view.bit_length % 8)) - 1)) == 0);
            assert((varying ? uqa_value_get_bit(value, &view) : uqa_value_get_varbit(value, &view)) == UQA_ERROR_TYPE);
            assert(view.data == NULL && view.size == 0 && view.bit_length == 0);
            CHECK(uqa_value_create_copy(value, &copy));
            CHECK(uqa_value_destroy(&value));
            parameter(engine, result, copy, cases[i].text, varying ? "bit varying" : "bit");
            CHECK(uqa_value_destroy(&copy));
        }
    }
    uqa_value invalid = NULL;
    uint8_t data = 0xff;
    assert(uqa_value_create_bit(&data, 1, 9, &invalid) == UQA_ERROR_INVALID_ARGUMENT && invalid == NULL);
    assert(uqa_value_create_varbit(&data, 1, 0, &invalid) == UQA_ERROR_INVALID_ARGUMENT && invalid == NULL);
    assert(uqa_value_create_bit(NULL, 1, 1, &invalid) == UQA_ERROR_INVALID_ARGUMENT && invalid == NULL);
    CHECK(uqa_engine_execute(engine, "SELECT NULL::bit(3), NULL::varbit, B'101'::bit(3), B'101'::varbit(8)", result));
    uqa_bits_view view = {NULL, 99, 99};
    assert(uqa_value_get_bit(cell(result, 0), &view) == UQA_ERROR_NULL && view.size == 0 && view.bit_length == 0);
    assert(uqa_value_get_varbit(cell(result, 1), &view) == UQA_ERROR_NULL && view.size == 0 && view.bit_length == 0);
    descriptor(cell(result, 2), UQA_TYPE_BIT, 1560, 3);
    descriptor(cell(result, 3), UQA_TYPE_VARBIT, 1562, 8);
    CHECK(uqa_value_get_bit(cell(result, 2), &view));
    assert(view.bit_length == 3 && view.size == 1 && view.data[0] == 0xa0);
    CHECK(uqa_value_get_varbit(cell(result, 3), &view));
    assert(view.bit_length == 3 && view.size == 1 && view.data[0] == 0xa0);
}

static void domains(uqa_engine engine, uqa_result result) {
    CHECK(uqa_engine_execute(engine,
        "CREATE DOMAIN packed_uuid AS uuid; CREATE DOMAIN packed_bit AS bit(3); CREATE DOMAIN packed_varbit AS varbit(8)", result));
    const char *types[] = {"packed_uuid", "packed_bit", "packed_varbit"};
    const char *texts[] = {"550e8400-e29b-41d4-a716-446655440000", "101", "101"};
    for (size_t i = 0; i < 3; ++i) {
        uqa_value value = NULL;
        uqa_type type = NULL, base = NULL;
        uqa_type_kind kind = UQA_TYPE_UNKNOWN;
        CHECK(uqa_value_create_from_text(engine, types[i], texts[i], &value));
        CHECK(uqa_value_get_type(value, &type));
        CHECK(uqa_type_get_kind(type, &kind));
        assert(kind == UQA_TYPE_DOMAIN);
        CHECK(uqa_type_get_base_type(type, &base));
        CHECK(uqa_type_get_kind(base, &kind));
        assert(kind == (i == 0 ? UQA_TYPE_UUID : i == 1 ? UQA_TYPE_BIT : UQA_TYPE_VARBIT));
        if (i == 0) {
            uqa_bytes_view view = {NULL, 0};
            CHECK(uqa_value_get_uuid_bytes(value, &view));
            assert(view.size == 16 && view.data[0] == 0x55);
        } else {
            uqa_bits_view view = {NULL, 0, 0};
            CHECK(i == 1 ? uqa_value_get_bit(value, &view) : uqa_value_get_varbit(value, &view));
            assert(view.size == 1 && view.bit_length == 3 && view.data[0] == 0xa0);
        }
        CHECK(uqa_value_destroy(&value));
    }
}

static void stored(uqa_engine engine, uqa_result result) {
    uqa_bytes_view uuid = {NULL, 0};
    uqa_bits_view bit = {NULL, 0, 0};
    uqa_value copy = NULL;
    CHECK(uqa_engine_execute(engine, "SELECT id, fixed, varying FROM packed_values", result));
    descriptor(cell(result, 0), UQA_TYPE_UUID, 2950, -1);
    descriptor(cell(result, 1), UQA_TYPE_BIT, 1560, 9);
    descriptor(cell(result, 2), UQA_TYPE_VARBIT, 1562, 15);
    CHECK(uqa_value_get_uuid_bytes(cell(result, 0), &uuid));
    assert(uuid.size == 16 && uuid.data[0] == 0x55 && uuid.data[15] == 0);
    CHECK(uqa_value_get_bit(cell(result, 1), &bit));
    assert(bit.bit_length == 9 && bit.size == 2 && bit.data[0] == 0 && bit.data[1] == 0x80);
    CHECK(uqa_value_get_varbit(cell(result, 2), &bit));
    assert(bit.bit_length == 15 && bit.size == 2 && bit.data[0] == 0x12 && bit.data[1] == 0x34);
    CHECK(uqa_value_create_copy(cell(result, 0), &copy));
    CHECK(uqa_engine_execute(engine, "SELECT 1", result));
    CHECK(uqa_value_get_uuid_bytes(copy, &uuid));
    assert(uuid.size == 16 && uuid.data[0] == 0x55);
    CHECK(uqa_value_destroy(&copy));
}

int main(void) {
    char path[4096];
    int length = snprintf(path, sizeof(path), "%s/packed-values", getenv("UQA_TEST_DATABASE_DIR"));
    assert(length > 0 && (size_t)length < sizeof(path));
    uqa_engine engine = NULL;
    uqa_result result = NULL;
    CHECK(test_database_open_path(path, NULL, &engine));
    CHECK(uqa_result_create(&result));
    uuids(engine, result);
    bits(engine, result);
    domains(engine, result);
    CHECK(uqa_engine_execute(engine, "CREATE TABLE packed_values(id uuid, fixed bit(9), varying varbit(15))", result));
    uqa_value values[3] = {NULL, NULL, NULL};
    uint8_t fixed[] = {0,0xff}, varying[] = {0x12,0x35};
    CHECK(uqa_value_create_uuid("550e8400-e29b-41d4-a716-446655440000", &values[0]));
    CHECK(uqa_value_create_bit(fixed, sizeof(fixed), 9, &values[1]));
    CHECK(uqa_value_create_varbit(varying, sizeof(varying), 15, &values[2]));
    uqa_statement statement = NULL;
    CHECK(uqa_statement_prepare(engine, "INSERT INTO packed_values VALUES ($1, $2, $3)", &statement));
    for (uint32_t i = 0; i < 3; ++i) {
        CHECK(uqa_statement_bind(statement, i + 1, values[i]));
        CHECK(uqa_value_destroy(&values[i]));
    }
    CHECK(uqa_statement_execute(statement, result));
    CHECK(uqa_statement_finalize(&statement));
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
