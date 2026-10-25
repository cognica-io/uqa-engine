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

static uqa_value_view cell(uqa_result result, uint32_t column) {
    uqa_value_view value = NULL;
    CHECK(uqa_result_get_value(result, 0, column, &value));
    return value;
}

static void values(uqa_result result, int has_void) {
    uint32_t oid = 0;
    uint8_t is_null = 1;
    uqa_string_view text = {NULL, 0};
    CHECK(uqa_value_get_oid(cell(result, 0), &oid));
    assert(oid == UINT32_MAX);
    CHECK(uqa_value_get_xid(cell(result, 1), &oid));
    assert(oid == UINT32_MAX);
    CHECK(uqa_value_get_xml(cell(result, 2), &text));
    assert(equal(text, "<?xml version=\"1.0\"?><a>世界</a><b/>"));
    if (has_void) {
        CHECK(uqa_value_get_void(cell(result, 3)));
        CHECK(uqa_value_get_is_null(cell(result, 3), &is_null));
        assert(is_null == 0);
    }
}

static void constructors(uqa_engine engine, uqa_result result) {
    uqa_value input[4] = {NULL};
    uqa_value copy = NULL;
    uqa_statement statement = NULL;
    uqa_cursor cursor = NULL;
    const char *types[] = {"oid", "xid", "xml", "void"};
    uint32_t number = 99;
    uqa_string_view text = {NULL, 0};
    CHECK(uqa_value_create_oid(UINT32_MAX, &input[0]));
    CHECK(uqa_value_create_xid(UINT32_MAX, &input[1]));
    CHECK(uqa_value_create_xml("<?xml version=\"1.0\"?><a>世界</a><b/>", &input[2]));
    CHECK(uqa_value_create_void(&input[3]));
    assert(uqa_value_get_xid(input[0], &number) == UQA_ERROR_TYPE && number == 0);
    assert(uqa_value_get_oid(input[1], &number) == UQA_ERROR_TYPE && number == 0);
    assert(uqa_value_get_xml(input[0], &text) == UQA_ERROR_TYPE && text.data == NULL && text.size == 0);
    assert(uqa_value_get_void(input[2]) == UQA_ERROR_TYPE);
    CHECK(uqa_value_create_copy(input[3], &copy));
    CHECK(uqa_value_destroy(&input[3]));
    input[3] = copy;
    copy = NULL;
    for (uint32_t index = 0; index < 4; ++index) {
        uqa_type type = NULL;
        CHECK(uqa_value_get_type(input[index], &type));
        CHECK(uqa_type_get_name(type, &text));
        assert(equal(text, types[index]));
    }
    {
        uqa_value_view params[] = {input[0], input[1], input[2], input[3]};
        CHECK(uqa_engine_execute_params(engine, "SELECT $1, $2, $3, $4", params, 4, result));
        values(result, 1);
        CHECK(uqa_statement_prepare_typed(engine, "SELECT $1, $2, $3, $4", types, 4, &statement));
        for (uint32_t index = 0; index < 4; ++index) CHECK(uqa_statement_bind(statement, index + 1, input[index]));
        CHECK(uqa_cursor_open_params(engine, "SELECT $1, $2, $3, $4", params, 4, &cursor));
        CHECK(uqa_engine_execute_params(engine, "INSERT INTO catalog_values VALUES ($1, $2, $3)", params, 3, result));
    }
    for (uint32_t index = 0; index < 4; ++index) CHECK(uqa_value_destroy(&input[index]));
    CHECK(uqa_statement_execute(statement, result));
    values(result, 1);
    CHECK(uqa_statement_finalize(&statement));
    CHECK(uqa_cursor_fetch(cursor, 1, result));
    values(result, 1);
    assert(uqa_cursor_fetch(cursor, 1, result) == UQA_DONE);
    CHECK(uqa_cursor_close(&cursor));
    CHECK(uqa_value_create_null(&copy));
    assert(uqa_value_get_void(copy) == UQA_ERROR_NULL);
    assert(uqa_value_get_xml(copy, &text) == UQA_ERROR_NULL && text.data == NULL);
    assert(uqa_value_get_oid(copy, &number) == UQA_ERROR_NULL && number == 0);
    CHECK(uqa_value_destroy(&copy));
    CHECK(uqa_value_create_text("<a/>", &copy));
    assert(uqa_value_get_xml(copy, &text) == UQA_ERROR_TYPE);
    assert(uqa_value_get_oid(copy, &number) == UQA_ERROR_TYPE);
    CHECK(uqa_value_destroy(&copy));
    {
        const char span[] = {'<', 'a', '/', '>'};
        CHECK(uqa_value_create_xml_n(span, sizeof(span), &copy));
        CHECK(uqa_value_get_xml(copy, &text));
        assert(equal(text, "<a/>"));
        CHECK(uqa_value_destroy(&copy));
    }
    assert(uqa_value_create_xml("<a>", &copy) == UQA_ERROR_SQL && copy == NULL);
    assert(uqa_value_create_xml(NULL, &copy) == UQA_ERROR_INVALID_ARGUMENT && copy == NULL);
    CHECK(uqa_value_create_oid(0, &copy));
    CHECK(uqa_value_get_oid(copy, &number));
    assert(number == 0);
    CHECK(uqa_value_destroy(&copy));
}

static void metadata(uqa_engine engine, uqa_result result) {
    /* PostgreSQL 18.4 pg_type defaults, independently captured in catalog_scalars.pg18.json. */
    const char *types[] = {"text", "name", "pg_node_tree", "xml", "void", "oid", "xid", "text[]", "c_collated", "c_collated[]"};
    const uint32_t collations[] = {100, 950, 100, 0, 0, 0, 0, 100, 950, 950};
    CHECK(uqa_engine_execute(engine, "CREATE DOMAIN c_collated AS text COLLATE \"C\"; CREATE DOMAIN c_oid AS oid; CREATE DOMAIN c_xml AS xml", result));
    for (size_t index = 0; index < sizeof(types) / sizeof(types[0]); ++index) {
        uqa_value value = NULL;
        uqa_type type = NULL;
        uint32_t oid = 99;
        CHECK(uqa_value_create_typed_null(engine, types[index], &value));
        CHECK(uqa_value_get_type(value, &type));
        assert(uqa_type_get_collation_oid(type, &oid) == (collations[index] == 0 ? UQA_DONE : UQA_OK));
        assert(oid == collations[index]);
        CHECK(uqa_value_destroy(&value));
    }
    CHECK(uqa_engine_execute(engine,
        "SELECT 4294967295::oid::c_oid, '<a/>'::xml::c_xml, 1::regproc, 1::regprocedure, "
        "1::regclass, 1::regnamespace, 1::regcollation, 1::regrole, 1::regtype", result));
    for (uint32_t index = 0; index < 9; ++index) {
        if (index == 1) {
            uqa_string_view text = {NULL, 0};
            CHECK(uqa_value_get_xml(cell(result, index), &text));
            assert(equal(text, "<a/>"));
        } else {
            uint32_t number = 0;
            CHECK(uqa_value_get_oid(cell(result, index), &number));
            assert(number == (index == 0 ? UINT32_MAX : 1));
        }
    }
}

static void character_and_catalog_text(uqa_engine engine, uqa_result result) {
    uqa_value value = NULL;
    uint8_t byte = 99;
    uqa_string_view text = {NULL, 0};
    for (uint32_t code = 0; code < 256; ++code) {
        uqa_value copy = NULL;
        CHECK(uqa_value_create_internal_char((uint8_t)code, &value));
        CHECK(uqa_value_get_internal_char(value, &byte));
        assert(byte == code);
        CHECK(uqa_value_create_copy(value, &copy));
        CHECK(uqa_value_destroy(&value));
        {
            uqa_value_view params[] = {copy};
            int32_t integer = 0;
            CHECK(uqa_engine_execute_params(engine, "SELECT $1, $1::integer", params, 1, result));
            CHECK(uqa_value_get_internal_char(cell(result, 0), &byte));
            CHECK(uqa_value_get_int32(cell(result, 1), &integer));
            assert(byte == code && integer == (code < 128 ? (int32_t)code : (int32_t)code - 256));
        }
        CHECK(uqa_value_destroy(&copy));
    }
    CHECK(uqa_value_create_text("a", &value));
    assert(uqa_value_get_internal_char(value, &byte) == UQA_ERROR_TYPE && byte == 0);
    CHECK(uqa_value_destroy(&value));
    CHECK(uqa_value_create_typed_null(engine, "pg_catalog.\"char\"", &value));
    assert(uqa_value_get_internal_char(value, &byte) == UQA_ERROR_NULL && byte == 0);
    CHECK(uqa_value_destroy(&value));
    CHECK(uqa_engine_execute(engine, "CREATE TABLE c_node_value(n integer DEFAULT 7); SELECT adbin FROM pg_attrdef WHERE adrelid='c_node_value'::regclass", result));
    CHECK(uqa_value_get_text(cell(result, 0), &text));
    assert(text.size > 0);
    CHECK(uqa_engine_execute(engine, "SELECT 'table'::name, 'portal'::refcursor, '=r/postgres'::aclitem", result));
    CHECK(uqa_value_get_text(cell(result, 0), &text));
    assert(equal(text, "table"));
    CHECK(uqa_value_get_text(cell(result, 1), &text));
    assert(equal(text, "portal"));
    CHECK(uqa_value_get_text(cell(result, 2), &text));
    assert(equal(text, "=r/postgres"));
}

int main(void) {
    uqa_engine engine = NULL;
    uqa_result result = NULL;
    char path[4096];
    int length = snprintf(path, sizeof(path), "%s/catalog-scalars", getenv("UQA_TEST_DATABASE_DIR"));
    assert(length > 0 && (size_t)length < sizeof(path));
    CHECK(test_database_open_path(path, NULL, &engine));
    CHECK(uqa_result_create(&result));
    CHECK(uqa_engine_execute(engine, "CREATE TABLE catalog_values(o oid, x xid, document xml)", result));
    constructors(engine, result);
    metadata(engine, result);
    character_and_catalog_text(engine, result);
    CHECK(uqa_engine_execute(engine, "SELECT o, x, document FROM catalog_values", result));
    values(result, 0);
    CHECK(uqa_result_destroy(&result));
    CHECK(uqa_engine_close(&engine));
    if (test_database_flags() != UQA_OPEN_MEMORY) {
        CHECK(test_database_open_path(path, NULL, &engine));
        CHECK(uqa_result_create(&result));
        CHECK(uqa_engine_execute(engine, "SELECT o, x, document FROM catalog_values", result));
        values(result, 0);
        CHECK(uqa_result_destroy(&result));
        CHECK(uqa_engine_close(&engine));
    }
    return 0;
}
