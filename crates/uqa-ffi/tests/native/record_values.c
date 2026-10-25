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

#ifdef __cplusplus
#define STATIC_ASSERT static_assert
#else
#define STATIC_ASSERT _Static_assert
#endif
STATIC_ASSERT(offsetof(uqa_field_descriptor, name) == 0, "field name layout");
STATIC_ASSERT(offsetof(uqa_field_descriptor, type) == sizeof(uqa_string_view), "field type layout");
STATIC_ASSERT(offsetof(uqa_field_descriptor, number) == sizeof(uqa_string_view) + sizeof(uqa_type), "field number layout");
STATIC_ASSERT(offsetof(uqa_field_descriptor, collation_oid) == offsetof(uqa_field_descriptor, number) + 4, "field collation layout");

static void checked(uqa_status status, int line) {
    if (status != UQA_OK) {
        uqa_error error = NULL;
        uqa_string_view message = {NULL, 0};
        if (uqa_error_get_last(&error) == UQA_OK) (void)uqa_error_get_message(error, &message);
        fprintf(stderr, "line %d: status %d: %.*s\n", line, (int)status,
                (int)message.size, message.data == NULL ? "" : message.data);
        abort();
    }
}
#define CHECK(call) checked((call), __LINE__)

static int equal(uqa_string_view actual, const char *expected) {
    return actual.size == strlen(expected) && (actual.size == 0 || memcmp(actual.data, expected, actual.size) == 0);
}

static void sql_error(uqa_status status, const char *expected) {
    uqa_error error = NULL;
    uqa_string_view state = {NULL, 0};
    assert(status == UQA_ERROR_SQL);
    CHECK(uqa_error_get_last(&error));
    CHECK(uqa_error_get_sqlstate(error, &state));
    assert(equal(state, expected));
}

static void formatted(uqa_value_view value, const char *expected) {
    uqa_buffer buffer = NULL;
    uqa_bytes_view text = {NULL, 0};
    CHECK(uqa_buffer_create(&buffer));
    CHECK(uqa_value_format_sql(value, buffer));
    CHECK(uqa_buffer_get_bytes(buffer, &text));
    if (text.size != strlen(expected) || memcmp(text.data, expected, text.size) != 0) {
        fprintf(stderr, "expected %s, got %.*s\n", expected, (int)text.size, (const char *)text.data);
        abort();
    }
    CHECK(uqa_buffer_destroy(&buffer));
}

static void field(uqa_type type, uint32_t index, const char *name, int32_t number,
                  uint32_t kind, int32_t modifier, uqa_field_descriptor *out) {
    uqa_field_descriptor again = {{NULL, 0}, NULL, 0, 0};
    uint32_t actual_kind = 0;
    int32_t actual_modifier = 0;
    CHECK(uqa_type_get_field(type, index, out));
    CHECK(uqa_type_get_field(type, index, &again));
    assert(out->name.data == again.name.data && out->type == again.type);
    assert(equal(out->name, name) && out->number == number);
    CHECK(uqa_type_get_kind(out->type, &actual_kind));
    CHECK(uqa_type_get_modifier(out->type, &actual_modifier));
    assert(actual_kind == kind && actual_modifier == modifier);
}

static void pair_type(uqa_type type) {
    uint32_t count = 0;
    uqa_field_descriptor member = {{NULL, 0}, NULL, 0, 0};
    CHECK(uqa_type_get_field_count(type, &count));
    assert(count == 4);
    field(type, 0, "nested", 1, UQA_TYPE_COMPOSITE, -1, &member);
    field(member.type, 0, "n", 1, UQA_TYPE_INT16, -1, &member);
    field(type, 1, "optional", 2, UQA_TYPE_INT32, -1, &member);
    field(type, 2, "numbers", 3, UQA_TYPE_ARRAY, -1, &member);
    field(type, 3, "amount", 4, UQA_TYPE_NUMERIC, 327686, &member);
    assert(uqa_type_get_field(type, count, &member) == UQA_ERROR_RANGE);
    assert(member.name.data == NULL && member.name.size == 0 && member.type == NULL && member.number == 0 && member.collation_oid == 0);
}

static void construction(uqa_engine engine, uqa_result result) {
    uqa_value number = NULL, text = NULL, leaf = NULL, null_value = NULL, array = NULL, amount = NULL;
    uqa_value pair = NULL, copy = NULL;
    uqa_type type = NULL;
    uqa_value_view value = NULL;
    uqa_statement statement = NULL;
    uqa_cursor cursor = NULL;
    CHECK(uqa_engine_execute(engine,
        "CREATE TYPE c_record_leaf AS (n smallint, label varchar(2)); "
        "CREATE TYPE c_record_pair AS (nested c_record_leaf, optional integer, numbers integer[], amount numeric(5,2)); "
        "CREATE TABLE c_record_data(v c_record_pair)", result));
    CHECK(uqa_value_create_int32(32767, &number));
    CHECK(uqa_value_create_text("abcd", &text));
    {
        uqa_value_view fields[] = {number, text};
        CHECK(uqa_value_create_record_n(engine, "c_record_leafSuffix", 13, fields, 2, &leaf));
    }
    CHECK(uqa_value_destroy(&number));
    CHECK(uqa_value_destroy(&text));
    CHECK(uqa_value_create_null(&null_value));
    CHECK(uqa_value_create_from_text(engine, "integer[]", "[0:1]={7,NULL}", &array));
    CHECK(uqa_value_create_text("12.3456", &amount));
    {
        uqa_value_view fields[] = {leaf, null_value, array, amount};
        CHECK(uqa_value_create_record(engine, "c_record_pair", fields, 4, &pair));
    }
    CHECK(uqa_value_destroy(&leaf));
    CHECK(uqa_value_destroy(&null_value));
    CHECK(uqa_value_destroy(&array));
    CHECK(uqa_value_destroy(&amount));
    formatted(pair, "(\"(32767,ab)\",,\"[0:1]={7,NULL}\",12.35)");
    CHECK(uqa_value_get_type(pair, &type));
    pair_type(type);
    CHECK(uqa_value_create_copy(pair, &copy));
    CHECK(uqa_value_destroy(&pair));
    value = copy;
    CHECK(uqa_engine_execute_params(engine, "INSERT INTO c_record_data VALUES ($1)", &value, 1, result));
    CHECK(uqa_statement_prepare(engine, "SELECT $1::c_record_pair", &statement));
    CHECK(uqa_statement_get_parameter_type(statement, 1, &type));
    pair_type(type);
    CHECK(uqa_statement_get_column_type(statement, 0, &type));
    pair_type(type);
    CHECK(uqa_statement_bind(statement, 1, copy));
    CHECK(uqa_cursor_open_params(engine, "SELECT $1", &value, 1, &cursor));
    CHECK(uqa_cursor_get_column_type(cursor, 0, &type));
    pair_type(type);
    CHECK(uqa_value_destroy(&copy));
    CHECK(uqa_statement_execute(statement, result));
    CHECK(uqa_result_get_column_type(result, 0, &type));
    pair_type(type);
    CHECK(uqa_result_get_value(result, 0, 0, &value));
    formatted(value, "(\"(32767,ab)\",,\"[0:1]={7,NULL}\",12.35)");
    CHECK(uqa_cursor_fetch(cursor, 1, result));
    CHECK(uqa_result_get_value(result, 0, 0, &value));
    formatted(value, "(\"(32767,ab)\",,\"[0:1]={7,NULL}\",12.35)");
    CHECK(uqa_statement_finalize(&statement));
    CHECK(uqa_cursor_close(&cursor));
}

static void catalog_changes(uqa_engine engine, uqa_result result) {
    uqa_value null_value = NULL;
    uqa_type type = NULL, domain = NULL;
    uqa_field_descriptor first = {{NULL, 0}, NULL, 0, 0}, changed = {{NULL, 0}, NULL, 0, 0};
    uint32_t count = 0, oid = 0;
    CHECK(uqa_engine_execute(engine,
        "CREATE TYPE c_fields AS (a integer, b text COLLATE \"C\", c numeric(7,2)); "
        "CREATE DOMAIN c_fields_domain AS c_fields", result));
    CHECK(uqa_value_create_typed_null(engine, "c_fields_domain", &null_value));
    CHECK(uqa_value_get_type(null_value, &domain));
    assert(uqa_type_get_field_count(domain, &count) == UQA_DONE && count == 0);
    CHECK(uqa_type_get_base_type(domain, &type));
    CHECK(uqa_type_get_field_count(type, &count));
    assert(count == 3);
    field(type, 0, "a", 1, UQA_TYPE_INT32, -1, &first);
    field(type, 1, "b", 2, UQA_TYPE_TEXT, -1, &changed);
    assert(changed.collation_oid == 950);
    CHECK(uqa_engine_execute(engine,
        "ALTER TYPE c_fields RENAME ATTRIBUTE a TO renamed; "
        "ALTER TYPE c_fields DROP ATTRIBUTE b; "
        "ALTER TYPE c_fields ADD ATTRIBUTE tail varchar(4)", result));
    CHECK(uqa_type_get_field_count(type, &count));
    assert(count == 3 && equal(first.name, "a"));
    field(type, 0, "renamed", 1, UQA_TYPE_INT32, -1, &changed);
    field(type, 1, "c", 3, UQA_TYPE_NUMERIC, 458758, &changed);
    field(type, 2, "tail", 4, UQA_TYPE_VARCHAR, 8, &changed);
    CHECK(uqa_transaction_begin(engine));
    CHECK(uqa_engine_execute(engine, "ALTER TYPE c_fields ALTER ATTRIBUTE renamed TYPE real", result));
    field(type, 0, "renamed", 1, UQA_TYPE_FLOAT32, -1, &changed);
    CHECK(uqa_transaction_rollback(engine));
    field(type, 0, "renamed", 1, UQA_TYPE_INT32, -1, &changed);
    CHECK(uqa_type_get_oid(first.type, &oid));
    assert(oid == 23 && equal(first.name, "a"));
    CHECK(uqa_engine_execute(engine, "DROP DOMAIN c_fields_domain; DROP TYPE c_fields", result));
    sql_error(uqa_type_get_field_count(type, &count), "42704");
    assert(count == 0);
    CHECK(uqa_type_get_oid(first.type, &oid));
    assert(oid == 23);
    CHECK(uqa_value_destroy(&null_value));
}

static int64_t calls(uqa_engine engine, uqa_result result) {
    uqa_value_view value = NULL;
    int64_t count = 0;
    CHECK(uqa_engine_execute(engine, "SELECT currval('record_calls')", result));
    CHECK(uqa_result_get_value(result, 0, 0, &value));
    CHECK(uqa_value_get_int64(value, &count));
    return count;
}

static void domains(uqa_engine engine, uqa_result result) {
    uqa_value first = NULL, second = NULL, row = NULL, missing = NULL;
    uqa_value_view fields[2];
    uint32_t state = UQA_TRANSACTION_IDLE;
    CHECK(uqa_engine_execute(engine,
        "CREATE SEQUENCE record_calls; "
        "CREATE DOMAIN c_record_counted AS integer CHECK (nextval('record_calls') > 0 AND VALUE > 0); "
        "CREATE TYPE c_record_checked AS (a c_record_counted, b c_record_counted); "
        "CREATE DOMAIN c_record_whole AS c_record_checked CHECK (nextval('record_calls') > 0); "
        "CREATE TYPE c_record_gone AS ENUM ('3')", result));
    CHECK(uqa_value_create_int32(2, &first));
    CHECK(uqa_value_create_int32(3, &second));
    fields[0] = first; fields[1] = second;
    CHECK(uqa_value_create_record(engine, "c_record_whole", fields, 2, &row));
    assert(calls(engine, result) == 3);
    {
        uqa_value_view parameter = row;
        CHECK(uqa_engine_execute_params(engine, "SELECT $1", &parameter, 1, result));
        CHECK(uqa_engine_execute_params(engine, "SELECT $1", &parameter, 1, result));
    }
    assert(calls(engine, result) == 3);
    CHECK(uqa_value_destroy(&row));
    sql_error(uqa_value_create_record(engine, "c_record_checked", fields, 1, &row), "42846");
    assert(row == NULL && calls(engine, result) == 3);
    CHECK(uqa_value_create_from_text(engine, "c_record_gone", "3", &missing));
    CHECK(uqa_engine_execute(engine, "DROP TYPE c_record_gone", result));
    fields[1] = missing;
    sql_error(uqa_value_create_record(engine, "c_record_checked", fields, 2, &row), "XX000");
    assert(row == NULL && calls(engine, result) == 3);
    CHECK(uqa_value_destroy(&missing));
    CHECK(uqa_value_destroy(&second));
    CHECK(uqa_value_create_int32(-1, &second));
    fields[1] = second;
    CHECK(uqa_transaction_begin(engine));
    CHECK(uqa_engine_execute(engine, "SAVEPOINT record_input", result));
    sql_error(uqa_value_create_record(engine, "c_record_whole", fields, 2, &row), "23514");
    assert(row == NULL);
    CHECK(uqa_transaction_get_state(engine, &state));
    assert(state == UQA_TRANSACTION_FAILED);
    CHECK(uqa_engine_execute(engine, "ROLLBACK TO record_input", result));
    assert(calls(engine, result) == 5);
    CHECK(uqa_transaction_rollback(engine));
    CHECK(uqa_value_destroy(&first));
    CHECK(uqa_value_destroy(&second));
}

static void invalid_and_empty(uqa_engine engine, uqa_result result) {
    uqa_value row = NULL;
    uqa_value_view invalid = NULL;
    uqa_type type = NULL;
    uqa_field_descriptor member = {{NULL, 0}, NULL, 0, 0};
    uint32_t count = 99;
    CHECK(uqa_engine_execute(engine, "CREATE TYPE c_record_empty AS ()", result));
    CHECK(uqa_value_create_record(engine, "c_record_empty", NULL, 0, &row));
    formatted(row, "()");
    CHECK(uqa_value_get_type(row, &type));
    CHECK(uqa_type_get_field_count(type, &count));
    assert(count == 0);
    assert(uqa_type_get_field(type, 0, &member) == UQA_ERROR_RANGE);
    CHECK(uqa_value_destroy(&row));
    assert(uqa_value_create_record_n(engine, NULL, 0, NULL, 0, &row) == UQA_ERROR_INVALID_ARGUMENT && row == NULL);
    assert(uqa_value_create_record(engine, "c_record_empty", &invalid, 1, &row) == UQA_ERROR_INVALID_ARGUMENT && row == NULL);
    assert(uqa_value_create_record(engine, "c_record_empty", NULL, 1, &row) == UQA_ERROR_INVALID_ARGUMENT && row == NULL);
    sql_error(uqa_value_create_record(engine, "integer", NULL, 0, &row), "42804");
    CHECK(uqa_value_create_record(engine, "record", NULL, 0, &row));
    formatted(row, "()");
    CHECK(uqa_value_destroy(&row));
    {
        uqa_value n = NULL, text = NULL;
        uqa_value_view fields[2];
        CHECK(uqa_value_create_int16(7, &n));
        CHECK(uqa_value_create_text("abc", &text));
        fields[0] = n; fields[1] = text;
        CHECK(uqa_value_create_record(engine, "record", fields, 2, &row));
        formatted(row, "(7,abc)");
        CHECK(uqa_value_destroy(&n));
        CHECK(uqa_value_destroy(&text));
        CHECK(uqa_value_destroy(&row));
    }
    CHECK(uqa_value_create_int32(1, &row));
    CHECK(uqa_value_get_type(row, &type));
    assert(uqa_type_get_field_count(type, &count) == UQA_DONE && count == 0);
    assert(uqa_type_get_field(type, 0, &member) == UQA_DONE && member.type == NULL);
    CHECK(uqa_value_destroy(&row));
    CHECK(uqa_value_create_typed_null(engine, "record", &row));
    CHECK(uqa_value_get_type(row, &type));
    assert(uqa_type_get_field_count(type, &count) == UQA_DONE && count == 0);
    CHECK(uqa_value_destroy(&row));
}

int main(void) {
    uqa_engine engine = NULL;
    uqa_result result = NULL;
    char path[4096];
    int length = snprintf(path, sizeof(path), "%s/record-values", getenv("UQA_TEST_DATABASE_DIR"));
    assert(length > 0 && (size_t)length < sizeof(path));
    CHECK(test_database_open_path(path, NULL, &engine));
    CHECK(uqa_result_create(&result));
    construction(engine, result);
    catalog_changes(engine, result);
    domains(engine, result);
    invalid_and_empty(engine, result);
    CHECK(uqa_result_destroy(&result));
    CHECK(uqa_engine_close(&engine));
    if (test_database_flags() != UQA_OPEN_MEMORY) {
        uqa_type type = NULL;
        uqa_value_view value = NULL;
        CHECK(test_database_open_path(path, NULL, &engine));
        CHECK(uqa_result_create(&result));
        CHECK(uqa_engine_execute(engine, "SELECT v FROM c_record_data", result));
        CHECK(uqa_result_get_column_type(result, 0, &type));
        pair_type(type);
        CHECK(uqa_result_get_value(result, 0, 0, &value));
        formatted(value, "(\"(32767,ab)\",,\"[0:1]={7,NULL}\",12.35)");
        CHECK(uqa_result_destroy(&result));
        CHECK(uqa_engine_close(&engine));
    }
    return 0;
}
