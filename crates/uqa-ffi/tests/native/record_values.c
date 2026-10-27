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

static void parameter_text(uqa_engine engine, uqa_result result, uqa_value_view parameter,
                           const char *sql, const char *expected) {
    uqa_value_view value = NULL;
    CHECK(uqa_engine_execute_params(engine, sql, &parameter, 1, result));
    CHECK(uqa_result_get_value(result, 0, 0, &value));
    formatted(value, expected);
}

static void retained_inputs(uqa_engine engine, uqa_result result) {
    uqa_value values[2] = {NULL, NULL};
    uqa_value n = NULL, text = NULL, tail = NULL, copy = NULL;
    uqa_statement statement = NULL;
    uqa_cursor cursor = NULL;
    uqa_value_view cell = NULL;
    CHECK(uqa_engine_execute(engine,
        "CREATE TYPE c_retained_input AS (a integer, b text, tail integer)", result));
    CHECK(uqa_value_create_from_text(engine, "c_retained_input", "(7,x,9)", &values[0]));
    CHECK(uqa_value_create_int32(7, &n));
    CHECK(uqa_value_create_text("x", &text));
    CHECK(uqa_value_create_int32(9, &tail));
    {
        uqa_value_view fields[] = {n, text, tail};
        CHECK(uqa_value_create_record(engine, "c_retained_input", fields, 3, &values[1]));
    }
    CHECK(uqa_value_destroy(&n)); CHECK(uqa_value_destroy(&text)); CHECK(uqa_value_destroy(&tail));
    CHECK(uqa_statement_prepare(engine, "SELECT ($1::c_retained_input).a::text", &statement));
    CHECK(uqa_statement_bind(statement, 1, values[1]));
    formatted(values[0], "(7,x,9)"); formatted(values[1], "(7,x,9)");
    CHECK(uqa_engine_execute(engine,
        "ALTER TYPE c_retained_input RENAME ATTRIBUTE a TO renamed", result));
    for (size_t i = 0; i < 2; ++i) {
        parameter_text(engine, result, values[i], "SELECT ($1).renamed::text", "7");
    }
    CHECK(uqa_statement_execute(statement, result));
    CHECK(uqa_result_get_value(result, 0, 0, &cell)); formatted(cell, "7");
    CHECK(uqa_statement_finalize(&statement));
    CHECK(uqa_engine_execute(engine,
        "ALTER TYPE c_retained_input DROP ATTRIBUTE b; "
        "ALTER TYPE c_retained_input ADD ATTRIBUTE added text", result));
    for (size_t i = 0; i < 2; ++i) {
        formatted(values[i], "(7,9,)");
        parameter_text(engine, result, values[i], "SELECT ($1).tail::text", "9");
    }
    CHECK(uqa_value_create_copy(values[0], &copy));
    CHECK(uqa_value_destroy(&values[0])); CHECK(uqa_value_destroy(&values[1]));
    cell = copy;
    CHECK(uqa_cursor_open_params(engine, "SELECT ($1).renamed::text", &cell, 1, &cursor));
    CHECK(uqa_cursor_fetch(cursor, 1, result));
    CHECK(uqa_result_get_value(result, 0, 0, &cell)); formatted(cell, "7");
    CHECK(uqa_cursor_close(&cursor));
    CHECK(uqa_engine_execute(engine, "CREATE TABLE c_retained_saved(v c_retained_input)", result));
    cell = copy;
    CHECK(uqa_engine_execute_params(engine, "INSERT INTO c_retained_saved VALUES ($1)", &cell, 1, result));
    CHECK(uqa_value_destroy(&copy));
}

static void retained_width(uqa_engine engine, uqa_result result) {
    uqa_value value = NULL, copy = NULL;
    uqa_value_view parameter = NULL;
    uint32_t state = UQA_TRANSACTION_IDLE;
    CHECK(uqa_engine_execute(engine, "CREATE TYPE c_retained_bits AS (a integer, b integer, c integer)", result));
    CHECK(uqa_value_create_from_text(engine, "c_retained_bits", "(16909060,84281096,287454020)", &value));
    CHECK(uqa_engine_execute(engine,
        "BEGIN; ALTER TYPE c_retained_bits ALTER ATTRIBUTE a TYPE bigint; SAVEPOINT before_read", result));
    parameter_text(engine, result, value, "SELECT ($1).a::text", "361984551007945476");
    parameter_text(engine, result, value, "SELECT ($1).b::text", "287454020");
    parameter_text(engine, result, value, "SELECT (($1).c IS NULL)::text", "false");
    CHECK(uqa_value_create_copy(value, &copy));
    CHECK(uqa_value_destroy(&value));
    parameter = copy;
    sql_error(uqa_engine_execute_params(engine, "SELECT ($1).c::text", &parameter, 1, result), "XX001");
    CHECK(uqa_transaction_get_state(engine, &state)); assert(state == UQA_TRANSACTION_FAILED);
    CHECK(uqa_engine_execute(engine, "ROLLBACK TO before_read", result));
    parameter_text(engine, result, copy, "SELECT ($1).a::text", "361984551007945476");
    CHECK(uqa_transaction_rollback(engine));
    formatted(copy, "(16909060,84281096,287454020)");
    CHECK(uqa_value_destroy(&copy));
}

static void retained_nested(uqa_engine engine, uqa_result result) {
    uqa_value child = NULL, second = NULL, array = NULL, named = NULL, anonymous = NULL;
    uqa_array_dimension dimension = {1, 1};
    uqa_value_view children[1], fields[2];
    CHECK(uqa_engine_execute(engine,
        "CREATE TYPE c_retained_child AS (a integer, b text); "
        "CREATE TYPE c_retained_parent AS (child c_retained_child, children c_retained_child[])", result));
    CHECK(uqa_value_create_from_text(engine, "c_retained_child", "(7,x)", &child));
    CHECK(uqa_value_create_from_text(engine, "c_retained_child", "(8,y)", &second));
    children[0] = second;
    CHECK(uqa_value_create_array(engine, "c_retained_child[]", &dimension, 1, children, 1, &array));
    fields[0] = child; fields[1] = array;
    CHECK(uqa_value_create_record(engine, "c_retained_parent", fields, 2, &named));
    CHECK(uqa_value_create_record(engine, "record", fields, 2, &anonymous));
    CHECK(uqa_value_destroy(&child)); CHECK(uqa_value_destroy(&second)); CHECK(uqa_value_destroy(&array));
    CHECK(uqa_engine_execute(engine, "ALTER TYPE c_retained_child RENAME ATTRIBUTE a TO renamed", result));
    parameter_text(engine, result, named, "SELECT (($1).child).renamed::text", "7");
    parameter_text(engine, result, named, "SELECT ((($1).children)[1]).renamed::text", "8");
    formatted(anonymous, "(\"(7,x)\",\"{\"\"(8,y)\"\"}\")");
    parameter_text(engine, result, anonymous, "SELECT $1::text", "(\"(7,x)\",\"{\"\"(8,y)\"\"}\")");
    {
        uqa_value_view parameter = anonymous;
        sql_error(uqa_engine_execute_params(engine, "SELECT (($1).f1).renamed::text", &parameter, 1, result), "42703");
        sql_error(uqa_engine_execute_params(engine, "SELECT ((($1).f2)[1]).renamed::text", &parameter, 1, result), "42703");
    }
    CHECK(uqa_value_destroy(&named)); CHECK(uqa_value_destroy(&anonymous));
}

static void retained_input_checks(uqa_engine engine, uqa_result result, unsigned field_check) {
    const char *type_name = field_check ? "c_retained_callback_pair" : "c_retained_callback_domain";
    for (unsigned structured = 0; structured != 2; ++structured) {
        uqa_value value = NULL;
        uqa_value_view count = NULL;
        if (field_check) {
            CHECK(uqa_engine_execute(engine,
                "CREATE SEQUENCE c_retained_callback_calls; "
                "CREATE DOMAIN c_retained_field_int AS integer; "
                "CREATE TYPE c_retained_callback_pair AS (a c_retained_field_int,b text); "
                "CREATE FUNCTION c_retained_callback_rename(v integer) "
                "RETURNS boolean LANGUAGE plpgsql AS $$BEGIN "
                "PERFORM nextval('c_retained_callback_calls'); "
                "ALTER TYPE c_retained_callback_pair RENAME ATTRIBUTE a TO renamed; RETURN true; END$$; "
                "ALTER DOMAIN c_retained_field_int ADD CONSTRAINT mutate "
                "CHECK(c_retained_callback_rename(VALUE))", result));
        } else {
            CHECK(uqa_engine_execute(engine,
                "CREATE SEQUENCE c_retained_callback_calls; "
                "CREATE TYPE c_retained_callback_pair AS (a integer,b text); "
                "CREATE FUNCTION c_retained_callback_rename(v c_retained_callback_pair) "
                "RETURNS boolean LANGUAGE plpgsql AS $$BEGIN "
                "PERFORM nextval('c_retained_callback_calls'); "
                "ALTER TYPE c_retained_callback_pair RENAME ATTRIBUTE a TO renamed; RETURN true; END$$; "
                "CREATE DOMAIN c_retained_callback_domain AS c_retained_callback_pair "
                "CHECK(c_retained_callback_rename(VALUE))", result));
        }
        if (structured) {
            uqa_value number = NULL, text = NULL;
            uqa_value_view fields[2];
            CHECK(uqa_value_create_int32(7, &number));
            CHECK(uqa_value_create_text("x", &text));
            fields[0] = number; fields[1] = text;
            CHECK(uqa_value_create_record(engine, type_name, fields, 2, &value));
            CHECK(uqa_value_destroy(&number)); CHECK(uqa_value_destroy(&text));
        } else {
            CHECK(uqa_value_create_from_text(engine, type_name, "(7,x)", &value));
        }
        formatted(value, "(7,x)");
        parameter_text(engine, result, value, "SELECT ($1).renamed::text", "7");
        CHECK(uqa_engine_execute(engine, "SELECT currval('c_retained_callback_calls')::text", result));
        CHECK(uqa_result_get_value(result, 0, 0, &count)); formatted(count, "1");
        CHECK(uqa_value_destroy(&value));
        if (field_check) {
            CHECK(uqa_engine_execute(engine,
                "ALTER DOMAIN c_retained_field_int DROP CONSTRAINT mutate; "
                "DROP FUNCTION c_retained_callback_rename(integer); "
                "DROP TYPE c_retained_callback_pair; DROP DOMAIN c_retained_field_int; "
                "DROP SEQUENCE c_retained_callback_calls", result));
        } else {
            CHECK(uqa_engine_execute(engine,
                "DROP DOMAIN c_retained_callback_domain; "
                "DROP FUNCTION c_retained_callback_rename(c_retained_callback_pair); "
                "DROP TYPE c_retained_callback_pair; DROP SEQUENCE c_retained_callback_calls", result));
        }
    }
}

static void field_values(uqa_engine engine, uqa_result result) {
    uqa_value value = NULL, copy = NULL;
    uqa_value_view first = NULL, next = NULL, old = NULL;
    uqa_type type = NULL;
    uint32_t count = 0, kind = 0;
    int32_t number = 0, modifier = 0;
    int64_t wide = 0;
    uint8_t is_null = 0;
    CHECK(uqa_engine_execute(engine, "CREATE TYPE c_field_values AS (a integer,b integer,c integer)", result));
    CHECK(uqa_value_create_from_text(engine, "c_field_values", "(1,2,3)", &value));
    CHECK(uqa_value_get_field_count(value, &count)); assert(count == 3);
    CHECK(uqa_value_get_field(value, 0, &first));
    CHECK(uqa_value_get_field(value, 1, &old));
    CHECK(uqa_value_get_int32(first, &number)); assert(number == 1);
    CHECK(uqa_value_get_field(value, 0, &next)); assert(first == next);
    CHECK(uqa_engine_execute(engine, "BEGIN; ALTER TYPE c_field_values ALTER ATTRIBUTE a TYPE bigint", result));
    CHECK(uqa_value_get_field(value, 0, &next)); assert(first != next);
    CHECK(uqa_value_get_int64(next, &wide)); assert(wide == INT64_C(8589934593));
    CHECK(uqa_value_get_int32(first, &number)); assert(number == 1);
    CHECK(uqa_value_get_field(value, 1, &next)); assert(old != next);
    CHECK(uqa_value_get_int32(next, &number)); assert(number == 3);
    CHECK(uqa_value_get_int32(old, &number)); assert(number == 2);
    CHECK(uqa_value_get_field(value, 2, &next));
    CHECK(uqa_value_get_is_null(next, &is_null)); assert(!is_null);
    sql_error(uqa_value_get_int32(next, &number), "XX001");
    CHECK(uqa_transaction_rollback(engine));
    CHECK(uqa_value_get_field(value, 0, &next)); assert(first == next);
    CHECK(uqa_engine_execute(engine,
        "ALTER TYPE c_field_values RENAME ATTRIBUTE a TO renamed; "
        "ALTER TYPE c_field_values DROP ATTRIBUTE b; "
        "ALTER TYPE c_field_values ADD ATTRIBUTE added varchar(3)", result));
    CHECK(uqa_value_get_field_count(value, &count)); assert(count == 3);
    CHECK(uqa_value_get_field(value, 0, &next)); formatted(next, "1");
    CHECK(uqa_value_get_field(value, 1, &next)); formatted(next, "3");
    CHECK(uqa_value_get_field(value, 2, &next));
    CHECK(uqa_value_get_is_null(next, &is_null)); assert(is_null);
    CHECK(uqa_value_get_type(next, &type));
    CHECK(uqa_type_get_kind(type, &kind)); assert(kind == UQA_TYPE_VARCHAR);
    CHECK(uqa_type_get_modifier(type, &modifier)); assert(modifier == 7);
    assert(uqa_value_get_field_count(next, &count) == UQA_ERROR_NULL && count == 0);
    assert(uqa_value_get_field(value, UINT32_MAX, &next) == UQA_ERROR_RANGE && next == NULL);
    count = 9;
    assert(uqa_value_get_field_count(first, &count) == UQA_ERROR_TYPE && count == 0);
    CHECK(uqa_value_create_copy(old, &copy));
    CHECK(uqa_value_destroy(&value));
    CHECK(uqa_value_get_int32(copy, &number)); assert(number == 2);
    CHECK(uqa_value_destroy(&copy));

    CHECK(uqa_engine_execute(engine, "SELECT ROW(0.1::real, NULL::numeric(7,-2), ARRAY[4,5])", result));
    CHECK(uqa_result_get_value(result, 0, 0, &first));
    CHECK(uqa_value_get_field_count(first, &count)); assert(count == 3);
    CHECK(uqa_value_get_field(first, 0, &next));
    { float real = 0; CHECK(uqa_value_get_float32(next, &real)); assert(real == 0.1f); }
    CHECK(uqa_value_get_field(first, 1, &next));
    CHECK(uqa_value_get_is_null(next, &is_null)); assert(is_null);
    CHECK(uqa_value_get_type(next, &type));
    CHECK(uqa_type_get_modifier(type, &modifier)); assert(modifier == 460802);
    CHECK(uqa_value_get_field(first, 2, &next));
    CHECK(uqa_value_get_element(next, 1, &old));
    CHECK(uqa_value_get_int32(old, &number)); assert(number == 5);
}

static void retained_output_fields(uqa_engine engine, uqa_result result) {
    const char *sql = "SELECT ROW(16909060,84281096,287454020)::c_field_output";
    uqa_result outputs[3] = {NULL, NULL, NULL};
    uqa_statement statement = NULL;
    uqa_cursor cursor = NULL;
    uqa_value_view values[3] = {NULL, NULL, NULL}, children[3] = {NULL, NULL, NULL};
    CHECK(uqa_engine_execute(engine, "CREATE TYPE c_field_output AS (a integer,b integer,c integer)", result));
    for (size_t i = 0; i < 3; ++i) CHECK(uqa_result_create(&outputs[i]));
    CHECK(uqa_engine_execute(engine, sql, outputs[0]));
    CHECK(uqa_statement_prepare(engine, sql, &statement));
    CHECK(uqa_statement_execute(statement, outputs[1]));
    CHECK(uqa_statement_finalize(&statement));
    CHECK(uqa_cursor_open(engine, sql, &cursor));
    CHECK(uqa_cursor_fetch(cursor, 1, outputs[2]));
    CHECK(uqa_cursor_close(&cursor));
    for (size_t i = 0; i < 3; ++i) {
        CHECK(uqa_result_get_value(outputs[i], 0, 0, &values[i]));
        CHECK(uqa_value_get_field(values[i], 0, &children[i]));
    }
    CHECK(uqa_engine_execute(engine, "BEGIN; ALTER TYPE c_field_output ALTER ATTRIBUTE a TYPE bigint", result));
    for (size_t i = 0; i < 3; ++i) {
        uqa_value_view current = NULL;
        int64_t wide = 0;
        int32_t original = 0;
        CHECK(uqa_value_get_field(values[i], 0, &current));
        CHECK(uqa_value_get_int64(current, &wide)); assert(wide == INT64_C(361984551007945476));
        CHECK(uqa_value_get_int32(children[i], &original)); assert(original == 16909060);
    }
    CHECK(uqa_transaction_rollback(engine));
    for (size_t i = 0; i < 3; ++i) CHECK(uqa_result_destroy(&outputs[i]));

    CHECK(uqa_engine_execute(engine,
        "CREATE FUNCTION c_field_mutate(v integer) RETURNS integer LANGUAGE plpgsql AS $$BEGIN "
        "ALTER TYPE c_field_output RENAME ATTRIBUTE a TO renamed; RETURN v; END$$", result));
    CHECK(uqa_engine_execute(engine, "SELECT ROW(7,c_field_mutate(8),9)::c_field_output", result));
    CHECK(uqa_result_get_value(result, 0, 0, &values[0]));
    CHECK(uqa_value_get_field(values[0], 0, &children[0])); formatted(children[0], "7");
    CHECK(uqa_value_get_field(values[0], 1, &children[0])); formatted(children[0], "8");

    CHECK(uqa_engine_execute(engine, "CREATE TABLE c_field_table(a integer,b integer,c integer); INSERT INTO c_field_table VALUES(1,2,3)", result));
    CHECK(uqa_result_create(&outputs[0]));
    CHECK(uqa_engine_execute(engine, "SELECT t FROM c_field_table t(alias_a)", outputs[0]));
    CHECK(uqa_result_get_value(outputs[0], 0, 0, &values[0]));
    CHECK(uqa_value_get_field(values[0], 0, &children[0]));
    CHECK(uqa_engine_execute(engine, "ALTER TABLE c_field_table RENAME COLUMN a TO renamed; ALTER TABLE c_field_table DROP COLUMN b", result));
    {
        uint32_t count = 0;
        uqa_value_view current = NULL;
        CHECK(uqa_value_get_field_count(values[0], &count)); assert(count == 2);
        CHECK(uqa_value_get_field(values[0], 0, &current)); formatted(current, "1");
        CHECK(uqa_value_get_field(values[0], 1, &current)); formatted(current, "3");
        formatted(children[0], "1");
    }
    CHECK(uqa_result_destroy(&outputs[0]));
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
    retained_inputs(engine, result);
    retained_width(engine, result);
    retained_nested(engine, result);
    retained_input_checks(engine, result, 0);
    retained_input_checks(engine, result, 1);
    field_values(engine, result);
    retained_output_fields(engine, result);
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
        CHECK(uqa_engine_execute(engine, "SELECT v::text FROM c_retained_saved", result));
        CHECK(uqa_result_get_value(result, 0, 0, &value));
        formatted(value, "(7,9,)");
        CHECK(uqa_result_destroy(&result));
        CHECK(uqa_engine_close(&engine));
    }
    return 0;
}
