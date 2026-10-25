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

static int equal(uqa_string_view text, const char *expected) {
    return text.size == strlen(expected) && memcmp(text.data, expected, text.size) == 0;
}

static int64_t integer(uqa_result result, uint32_t column) {
    uqa_value_view value = NULL;
    int64_t number = 0;
    CHECK(uqa_result_get_value(result, 0, column, &value));
    CHECK(uqa_value_get_int64(value, &number));
    return number;
}

static void sqlstate(const char *expected) {
    uqa_error error = NULL;
    uqa_string_view state = {NULL, 0};
    CHECK(uqa_error_get_last(&error));
    CHECK(uqa_error_get_sqlstate(error, &state));
    assert(equal(state, expected));
}

static void empty_result(uqa_result result) {
    uint64_t rows = 99;
    uint32_t columns = 99;
    uqa_string_view tag = {NULL, 0};
    CHECK(uqa_result_get_row_count(result, &rows));
    CHECK(uqa_result_get_column_count(result, &columns));
    assert(rows == 0 && columns == 0);
    assert(uqa_result_get_command_tag(result, &tag) == UQA_DONE);
    assert(tag.data == NULL && tag.size == 0);
}

static void prepared_recovery(uqa_engine engine, uqa_result result) {
    uqa_statement statement = NULL;
    uqa_statement typed_exit = NULL;
    uqa_value value = NULL;
    uint32_t state = 0;
    const char *integer_type[] = {"integer"};
    const char *missing_type[] = {"missing_prepared_type"};
    const char *domain_type[] = {"c_admission_positive"};

    CHECK(uqa_statement_prepare_typed(engine, "ROLLBACK", integer_type, 1, &typed_exit));
    CHECK(uqa_value_create_int64(1, &value));
    CHECK(uqa_statement_bind(typed_exit, 1, value));
    CHECK(uqa_value_destroy(&value));
    CHECK(uqa_engine_execute(engine, "BEGIN; SAVEPOINT retry", result));
    assert(uqa_statement_prepare_typed(engine, "", missing_type, 1, &statement) == UQA_ERROR_SQL);
    assert(statement == NULL);
    sqlstate("42704");
    CHECK(uqa_transaction_get_state(engine, &state));
    assert(state == UQA_TRANSACTION_FAILED);
    assert(uqa_statement_execute(typed_exit, result) == UQA_ERROR_SQL);
    sqlstate("25P02");
    CHECK(uqa_statement_prepare(engine, "ROLLBACK TO retry", &statement));
    CHECK(uqa_statement_execute(statement, result));
    CHECK(uqa_transaction_get_state(engine, &state));
    assert(state == UQA_TRANSACTION_ACTIVE);
    CHECK(uqa_statement_finalize(&statement));
    assert(uqa_engine_execute(engine, "SELECT 1/0", result) == UQA_ERROR_SQL);
    CHECK(uqa_statement_prepare(engine, "COMMIT", &statement));
    CHECK(uqa_statement_execute(statement, result));
    CHECK(uqa_transaction_get_state(engine, &state));
    assert(state == UQA_TRANSACTION_IDLE);
    CHECK(uqa_statement_finalize(&statement));
    CHECK(uqa_statement_finalize(&typed_exit));

    CHECK(uqa_engine_execute(engine, "CREATE DOMAIN c_admission_positive AS integer CHECK (VALUE > 0)", result));
    CHECK(uqa_statement_prepare_typed(engine, "ROLLBACK", domain_type, 1, &statement));
    CHECK(uqa_value_create_int64(0, &value));
    CHECK(uqa_statement_bind(statement, 1, value));
    CHECK(uqa_value_destroy(&value));
    assert(uqa_statement_execute(statement, result) == UQA_ERROR_SQL);
    sqlstate("23514");
    CHECK(uqa_transaction_get_state(engine, &state));
    assert(state == UQA_TRANSACTION_IDLE);
    CHECK(uqa_value_create_int64(1, &value));
    CHECK(uqa_statement_bind(statement, 1, value));
    CHECK(uqa_value_destroy(&value));
    CHECK(uqa_statement_execute(statement, result));
    CHECK(uqa_statement_finalize(&statement));
    CHECK(uqa_engine_execute(engine, "DROP DOMAIN c_admission_positive", result));
}

static void procedure_statements(uqa_engine engine, uqa_result result) {
    uqa_statement statement = NULL;
    uqa_value value = NULL;
    uqa_type type = NULL;
    uqa_string_view name = {NULL, 0};
    uint32_t count = 0;
    uint32_t kind = 0;
    CHECK(uqa_engine_execute(engine,
        "CREATE PROCEDURE c_change_value(IN amount integer, INOUT value integer, OUT label text) "
        "LANGUAGE plpgsql AS $$ BEGIN value := value + amount; label := 'changed'; END $$", result));
    CHECK(uqa_statement_prepare(engine, "CALL c_change_value(label => NULL, value => $1, amount => $2)", &statement));
    CHECK(uqa_statement_get_parameter_count(statement, &count));
    assert(count == 2);
    CHECK(uqa_statement_get_parameter_type(statement, 1, &type));
    CHECK(uqa_type_get_kind(type, &kind));
    assert(kind == UQA_TYPE_INT32);
    CHECK(uqa_statement_get_column_count(statement, &count));
    assert(count == 2);
    CHECK(uqa_statement_get_column_name(statement, 0, &name));
    assert(equal(name, "value"));
    CHECK(uqa_statement_get_column_name(statement, 1, &name));
    assert(equal(name, "label"));
    CHECK(uqa_statement_get_column_type(statement, 1, &type));
    CHECK(uqa_type_get_kind(type, &kind));
    assert(kind == UQA_TYPE_TEXT);
    CHECK(uqa_value_create_int64(11, &value));
    CHECK(uqa_statement_bind(statement, 1, value));
    CHECK(uqa_value_destroy(&value));
    CHECK(uqa_value_create_int64(2, &value));
    CHECK(uqa_statement_bind(statement, 2, value));
    CHECK(uqa_value_destroy(&value));
    CHECK(uqa_statement_execute(statement, result));
    assert(integer(result, 0) == 13);
    CHECK(uqa_engine_execute(engine, "DROP PROCEDURE c_change_value(integer, integer)", result));
    assert(uqa_statement_execute(statement, result) == UQA_ERROR_SQL);
    sqlstate("42883");
    CHECK(uqa_statement_finalize(&statement));
}

static void xml_values(uqa_engine engine, uqa_result result) {
    uqa_statement statement = NULL;
    uqa_type type = NULL;
    uqa_type_kind kind = 0;
    uqa_value_view value = NULL;
    uqa_string_view text = {NULL, 0};
    uint32_t oid = 0;
    uint8_t is_null = 0;
    CHECK(uqa_engine_execute(engine, "SET xmloption TO content", result));
    CHECK(uqa_statement_prepare(engine, "SELECT '<?xml version=\"1.0\"?><a/>'::xml, NULL::xml", &statement));
    CHECK(uqa_statement_get_column_type(statement, 0, &type));
    CHECK(uqa_type_get_kind(type, &kind));
    CHECK(uqa_type_get_oid(type, &oid));
    assert(kind == UQA_TYPE_XML && oid == 142);
    CHECK(uqa_statement_execute(statement, result));
    CHECK(uqa_result_get_column_type(result, 0, &type));
    CHECK(uqa_type_get_kind(type, &kind));
    assert(kind == UQA_TYPE_XML);
    CHECK(uqa_result_get_value(result, 0, 0, &value));
    CHECK(uqa_value_get_text(value, &text));
    assert(equal(text, "<?xml version=\"1.0\"?><a/>"));
    CHECK(uqa_result_get_value(result, 0, 1, &value));
    CHECK(uqa_value_get_is_null(value, &is_null));
    assert(is_null == 1);
    CHECK(uqa_statement_finalize(&statement));
    CHECK(uqa_engine_execute(engine, "BEGIN; SAVEPOINT xml_retry; SET LOCAL xmloption TO document", result));
    assert(uqa_engine_execute(engine, "SELECT 'text'::xml", result) == UQA_ERROR_SQL);
    sqlstate("2200M");
    CHECK(uqa_engine_execute(engine, "ROLLBACK TO xml_retry; SELECT 'text'::xml", result));
    CHECK(uqa_result_get_value(result, 0, 0, &value));
    CHECK(uqa_value_get_text(value, &text));
    assert(equal(text, "text"));
    CHECK(uqa_engine_execute(engine, "ROLLBACK", result));
}

static void explained_inputs(uqa_engine engine, uqa_result result) {
    uqa_statement statement = NULL;
    uqa_value value = NULL;
    uqa_type type = NULL;
    uint32_t count = 0;
    uint32_t kind = 0;
    uint32_t state = 0;
    CHECK(uqa_engine_execute(engine, "CREATE TABLE c_explain_inputs (amount integer)", result));
    CHECK(uqa_statement_prepare(engine, "EXPLAIN ANALYZE INSERT INTO c_explain_inputs VALUES ($1)", &statement));
    CHECK(uqa_statement_get_parameter_count(statement, &count));
    assert(count == 1);
    CHECK(uqa_statement_get_parameter_type(statement, 1, &type));
    CHECK(uqa_type_get_kind(type, &kind));
    assert(kind == UQA_TYPE_INT32);
    CHECK(uqa_engine_execute(engine, "SELECT count(*) FROM c_explain_inputs", result));
    assert(integer(result, 0) == 0);
    CHECK(uqa_value_create_int64(7, &value));
    CHECK(uqa_statement_bind(statement, 1, value));
    CHECK(uqa_value_destroy(&value));
    CHECK(uqa_statement_execute(statement, result));
    CHECK(uqa_statement_finalize(&statement));
    CHECK(uqa_engine_execute(engine, "SELECT amount FROM c_explain_inputs", result));
    assert(integer(result, 0) == 7);

    CHECK(uqa_engine_execute(engine, "BEGIN; SAVEPOINT explain_retry", result));
    assert(uqa_statement_prepare(engine, "EXPLAIN UPDATE c_explain_inputs SET amount = 'bad'", &statement) == UQA_ERROR_SQL);
    assert(statement == NULL);
    sqlstate("22P02");
    CHECK(uqa_transaction_get_state(engine, &state));
    assert(state == UQA_TRANSACTION_FAILED);
    CHECK(uqa_engine_execute(engine, "ROLLBACK TO explain_retry", result));
    CHECK(uqa_transaction_get_state(engine, &state));
    assert(state == UQA_TRANSACTION_ACTIVE);
    CHECK(uqa_engine_execute(engine, "UPDATE c_explain_inputs SET amount = 9; ROLLBACK", result));
    CHECK(uqa_engine_execute(engine, "SELECT amount FROM c_explain_inputs", result));
    assert(integer(result, 0) == 7);
    CHECK(uqa_engine_execute(engine, "DROP TABLE c_explain_inputs", result));
}

static void explained_results(uqa_engine engine, uqa_result result) {
    const struct { const char *name; uint32_t kind; uint32_t oid; } formats[] = {
        {"text", UQA_TYPE_TEXT, 25},
        {"json", UQA_TYPE_JSON, 114},
        {"xml", UQA_TYPE_XML, 142},
        {"yaml", UQA_TYPE_TEXT, 25}
    };
    for (size_t index = 0; index < sizeof(formats) / sizeof(formats[0]); index++) {
        char sql[128];
        uqa_statement statement = NULL;
        uqa_type type = NULL;
        uqa_value_view value = NULL;
        uqa_string_view text = {NULL, 0};
        uint32_t columns = 0, kind = 0, oid = 0;
        uint64_t rows = 0;
        (void)snprintf(sql, sizeof(sql), "EXPLAIN (FORMAT %s) SELECT 1", formats[index].name);
        CHECK(uqa_statement_prepare(engine, sql, &statement));
        CHECK(uqa_statement_get_column_count(statement, &columns));
        assert(columns == 1);
        CHECK(uqa_statement_get_column_name(statement, 0, &text));
        assert(equal(text, "QUERY PLAN"));
        CHECK(uqa_statement_get_column_type(statement, 0, &type));
        CHECK(uqa_type_get_kind(type, &kind));
        CHECK(uqa_type_get_oid(type, &oid));
        assert(kind == formats[index].kind && oid == formats[index].oid);
        CHECK(uqa_statement_execute(statement, result));
        CHECK(uqa_result_get_column_type(result, 0, &type));
        CHECK(uqa_type_get_kind(type, &kind));
        CHECK(uqa_type_get_oid(type, &oid));
        assert(kind == formats[index].kind && oid == formats[index].oid);
        CHECK(uqa_result_get_row_count(result, &rows));
        assert(rows > 0 && (index == 0 || rows == 1));
        CHECK(uqa_result_get_value(result, 0, 0, &value));
        if (kind == UQA_TYPE_JSON) {
            uqa_buffer buffer = NULL;
            uqa_bytes_view bytes = {NULL, 0};
            assert(uqa_value_get_text(value, &text) == UQA_ERROR_TYPE);
            CHECK(uqa_buffer_create(&buffer));
            CHECK(uqa_value_format_sql(value, buffer));
            CHECK(uqa_buffer_get_bytes(buffer, &bytes));
            assert(bytes.size > 0 && bytes.data[0] == '[');
            CHECK(uqa_buffer_destroy(&buffer));
        } else {
            CHECK(uqa_value_get_text(value, &text));
            assert(text.size > 0);
            if (index == 2) assert(text.data[0] == '<');
            if (index == 3) assert(text.data[0] == '-');
        }
        CHECK(uqa_statement_finalize(&statement));
    }
    {
        uqa_statement statement = NULL;
        uqa_type type = NULL;
        uint32_t kind = 0;
        CHECK(uqa_engine_execute(engine, "CREATE TEMP SEQUENCE c_explain_effect", result));
        CHECK(uqa_statement_prepare(engine,
            "EXPLAIN (ANALYZE, FORMAT 'JSON') SELECT nextval('c_explain_effect')", &statement));
        CHECK(uqa_statement_get_column_type(statement, 0, &type));
        CHECK(uqa_type_get_kind(type, &kind));
        assert(kind == UQA_TYPE_TEXT);
        assert(uqa_statement_execute(statement, result) == UQA_ERROR_SQL);
        sqlstate("22023");
        CHECK(uqa_engine_execute(engine, "SELECT nextval('c_explain_effect')", result));
        assert(integer(result, 0) == 1);
        CHECK(uqa_statement_finalize(&statement));
        CHECK(uqa_engine_execute(engine, "BEGIN; SAVEPOINT retry", result));
        CHECK(uqa_statement_prepare(engine,
            "EXPLAIN (ANALYZE 'yes') SELECT nextval('c_explain_effect')", &statement));
        assert(uqa_statement_execute(statement, result) == UQA_ERROR_SQL);
        sqlstate("42601");
        CHECK(uqa_engine_execute(engine, "ROLLBACK TO retry", result));
        CHECK(uqa_engine_execute(engine, "SELECT nextval('c_explain_effect')", result));
        assert(integer(result, 0) == 2);
        CHECK(uqa_engine_execute(engine, "ROLLBACK", result));
        CHECK(uqa_statement_finalize(&statement));
    }
}

static void explained_stream_and_settings(uqa_engine engine, uqa_result result) {
    uqa_statement statement = NULL;
    uqa_value snapshot = NULL;
    uqa_value_view view = NULL;
    uqa_string_view text = {NULL, 0};
    uint8_t is_null = 0;
    CHECK(uqa_engine_execute(engine, "SET work_mem TO '64kB'", result));
    CHECK(uqa_statement_prepare(engine,
        "EXPLAIN (ANALYZE, SETTINGS, SUMMARY FALSE, TIMING FALSE, FORMAT JSON) "
        "SELECT repeat('x', 4096) || n::text FROM generate_series(1, 1024) AS g(n)", &statement));
    CHECK(uqa_statement_execute(statement, result));
    CHECK(uqa_result_get_value(result, 0, 0, &view));
    CHECK(uqa_value_create_copy(view, &snapshot));
    CHECK(uqa_statement_finalize(&statement));
    view = snapshot;
    CHECK(uqa_engine_execute_params(engine,
        "SELECT ($1->0->'Plan'->>'Actual Rows')::bigint, "
        "$1->0->'Settings'->>'work_mem', $1->0->>'Planning Time', $1->0->>'Execution Time'",
        &view, 1, result));
    CHECK(uqa_value_destroy(&snapshot));
    assert(integer(result, 0) == 1024);
    CHECK(uqa_result_get_value(result, 0, 1, &view));
    CHECK(uqa_value_get_text(view, &text));
    assert(equal(text, "64kB"));
    for (uint32_t column = 2; column < 4; column++) {
        CHECK(uqa_result_get_value(result, 0, column, &view));
        CHECK(uqa_value_get_is_null(view, &is_null));
        assert(is_null);
    }
    CHECK(uqa_engine_execute(engine, "RESET work_mem", result));
}

static void explained_costs(uqa_engine engine, uqa_result result) {
    uqa_statement statement = NULL;
    uqa_result inspected = NULL;
    uqa_value document = NULL;
    uqa_value_view view = NULL;
    CHECK(uqa_result_create(&inspected));
    for (int enabled = 0; enabled < 2; ++enabled) {
        const char *sql = enabled
            ? "EXPLAIN (FORMAT JSON, COSTS TRUE) SELECT 1::smallint, 2::integer, 3::bigint, true"
            : "EXPLAIN (FORMAT JSON, COSTS FALSE) SELECT 1::smallint, 2::integer, 3::bigint, true";
        CHECK(uqa_statement_prepare(engine, sql, &statement));
        CHECK(uqa_statement_execute(statement, result));
        CHECK(uqa_statement_finalize(&statement));
        CHECK(uqa_result_get_value(result, 0, 0, &view));
        CHECK(uqa_value_create_copy(view, &document));
        CHECK(uqa_statement_prepare(engine,
            "SELECT ($1::json->0->'Plan'->>'Plan Width')::integer, "
            "($1::json->0->'Plan'->>'Plan Rows')::numeric::integer, "
            "CASE WHEN ($1::json->0->'Plan'->>'Total Cost')::numeric >= "
            "($1::json->0->'Plan'->>'Startup Cost')::numeric THEN 1 ELSE 0 END",
            &statement));
        CHECK(uqa_statement_bind(statement, 1, document));
        CHECK(uqa_value_destroy(&document));
        CHECK(uqa_statement_execute(statement, inspected));
        if (enabled) {
            assert(integer(inspected, 0) == 15 && integer(inspected, 1) == 1);
            assert(integer(inspected, 2) == 1);
        } else {
            uint8_t is_null = 0;
            CHECK(uqa_result_get_value(inspected, 0, 0, &view));
            CHECK(uqa_value_get_is_null(view, &is_null));
            assert(is_null == 1 && integer(inspected, 2) == 0);
        }
        CHECK(uqa_statement_finalize(&statement));
    }
    CHECK(uqa_result_destroy(&inspected));
}

static void explained_named_inputs(uqa_engine engine, uqa_result result) {
    uqa_statement statement = NULL;
    uqa_value document = NULL;
    uqa_value_view view = NULL;
    CHECK(uqa_engine_execute(engine,
        "CREATE TEMP SEQUENCE c_named_effect; "
        "CREATE TEMP VIEW c_named_input AS SELECT n, nextval('c_named_effect') AS effect "
        "FROM (VALUES (1),(2),(3)) input(n)", result));
    for (int analyze = 0; analyze < 2; ++analyze) {
        CHECK(uqa_engine_execute(engine, "ALTER SEQUENCE c_named_effect RESTART WITH 1", result));
        CHECK(uqa_statement_prepare(engine, analyze
            ? "EXPLAIN (ANALYZE, TIMING FALSE, FORMAT JSON) SELECT n FROM c_named_input"
            : "EXPLAIN (FORMAT JSON) SELECT n FROM c_named_input", &statement));
        CHECK(uqa_statement_execute(statement, result));
        CHECK(uqa_statement_finalize(&statement));
        CHECK(uqa_result_get_value(result, 0, 0, &view));
        CHECK(uqa_value_create_copy(view, &document));
        const uqa_value_view params[] = {document};
        CHECK(uqa_engine_execute_params(engine,
            "SELECT ($1::json->0->'Plan'->>'Plan Rows')::numeric::integer, "
            "($1::json->0->'Plan'->>'Plan Width')::integer, "
            "COALESCE(($1::json->0->'Plan'->>'Actual Rows')::numeric::integer,-1)",
            params, 1, result));
        assert(integer(result, 0) == 3 && integer(result, 1) == 4);
        assert(integer(result, 2) == (analyze ? 3 : -1));
        CHECK(uqa_value_destroy(&document));
        CHECK(uqa_engine_execute(engine, "SELECT nextval('c_named_effect')", result));
        assert(integer(result, 0) == (analyze ? 4 : 1));
    }
    CHECK(uqa_engine_execute(engine,
        "DROP VIEW c_named_input; DROP SEQUENCE c_named_effect", result));
}

static void explained_serialization(uqa_engine engine, uqa_result result) {
    for (int binary = 0; binary < 2; ++binary) {
        uqa_statement statement = NULL;
        uqa_value document = NULL;
        uqa_value_view view = NULL;
        CHECK(uqa_engine_execute(engine, "CREATE TEMP SEQUENCE c_serialized_effect", result));
        CHECK(uqa_statement_prepare(engine, binary
            ? "EXPLAIN (ANALYZE, SERIALIZE BINARY, TIMING FALSE, FORMAT JSON) SELECT nextval('c_serialized_effect') FROM generate_series(1,257)"
            : "EXPLAIN (ANALYZE, SERIALIZE TEXT, TIMING FALSE, FORMAT JSON) SELECT nextval('c_serialized_effect') FROM generate_series(1,257)", &statement));
        CHECK(uqa_statement_execute(statement, result));
        CHECK(uqa_statement_finalize(&statement));
        CHECK(uqa_result_get_value(result, 0, 0, &view));
        CHECK(uqa_value_create_copy(view, &document));
        view = document;
        CHECK(uqa_engine_execute_params(engine,
            "SELECT ($1->0->'Serialization'->>'Output Volume')::bigint, "
            "CASE WHEN $1->0->'Serialization'->>'Time' IS NULL THEN 1 ELSE 0 END, "
            "($1->0->'Plan'->>'Actual Rows')::bigint", &view, 1, result));
        CHECK(uqa_value_destroy(&document));
        assert(integer(result, 0) == (binary ? 4 : 3));
        assert(integer(result, 1) == 1 && integer(result, 2) == 257);
        CHECK(uqa_engine_execute(engine, "SELECT nextval('c_serialized_effect')", result));
        assert(integer(result, 0) == 258);
        CHECK(uqa_engine_execute(engine, "DROP SEQUENCE c_serialized_effect", result));
    }
    CHECK(uqa_engine_execute(engine,
        "CREATE TEMP SEQUENCE c_serialized_rows; CREATE TEMP TABLE c_serialized_target(n bigint)",
        result));
    CHECK(uqa_engine_execute(engine, "BEGIN; SAVEPOINT before_serialized_output", result));
    assert(uqa_engine_execute(engine,
        "EXPLAIN (ANALYZE, SERIALIZE BINARY) INSERT INTO c_serialized_target "
        "SELECT nextval('c_serialized_rows') FROM generate_series(1,8) RETURNING n, NULL::aclitem",
        result) == UQA_ERROR_SQL);
    sqlstate("42883");
    CHECK(uqa_engine_execute(engine, "ROLLBACK TO before_serialized_output", result));
    CHECK(uqa_engine_execute(engine,
        "SELECT currval('c_serialized_rows'), count(*) FROM c_serialized_target", result));
    assert(integer(result, 0) == 1 && integer(result, 1) == 0);
    CHECK(uqa_engine_execute(engine, "ROLLBACK", result));
    CHECK(uqa_engine_execute(engine,
        "INSERT INTO c_serialized_target SELECT generate_series(1,8); "
        "BEGIN; SAVEPOINT before_serialized_delete", result));
    assert(uqa_engine_execute(engine,
        "EXPLAIN (ANALYZE, SERIALIZE BINARY) DELETE FROM c_serialized_target "
        "WHERE nextval('c_serialized_rows')>0 RETURNING NULL::aclitem",
        result) == UQA_ERROR_SQL);
    sqlstate("42883");
    CHECK(uqa_engine_execute(engine, "ROLLBACK TO before_serialized_delete", result));
    CHECK(uqa_engine_execute(engine,
        "SELECT currval('c_serialized_rows'), count(*) FROM c_serialized_target", result));
    assert(integer(result, 0) == 2 && integer(result, 1) == 8);
    CHECK(uqa_engine_execute(engine, "ROLLBACK", result));
    CHECK(uqa_engine_execute(engine, "DROP TABLE c_serialized_target; DROP SEQUENCE c_serialized_rows", result));
}

static void explained_physical_metadata(uqa_engine engine, uqa_result result) {
    CHECK(uqa_engine_execute(engine,
        "CREATE TABLE c_explain_vectors (id integer, embedding tensor(2)); "
        "INSERT INTO c_explain_vectors VALUES (1, ARRAY[ARRAY[1.0,0.0]]), (2, ARRAY[ARRAY[0.0,1.0]]); "
        "CREATE INDEX c_explain_vector_index ON c_explain_vectors USING diskann(embedding) "
        "WITH(max_degree=2,search_list_size=4,beam_width=1,pq_bytes=1)", result));
    for (unsigned costs = 0; costs < 2; costs++) {
        char sql[320];
        uqa_statement statement = NULL;
        uqa_value snapshot = NULL;
        uqa_value_view view = NULL;
        uqa_string_view text = {NULL, 0};
        uint8_t is_null = 0;
        (void)snprintf(sql, sizeof(sql),
            "EXPLAIN (FORMAT JSON, COSTS %s) WITH picked AS NOT MATERIALIZED "
            "(SELECT id FROM c_explain_vectors d WHERE knn_match(d.embedding,ARRAY[1.0,0.0],2)) "
            "SELECT id FROM picked", costs ? "TRUE" : "FALSE");
        CHECK(uqa_statement_prepare(engine, sql, &statement));
        CHECK(uqa_statement_execute(statement, result));
        CHECK(uqa_result_get_value(result, 0, 0, &view));
        CHECK(uqa_value_create_copy(view, &snapshot));
        CHECK(uqa_statement_finalize(&statement));
        view = snapshot;
        CHECK(uqa_engine_execute_params(engine,
            "SELECT $1->0->'Physical Plans'->0->>'Field', "
            "$1->0->'Physical Plans'->0->>'Route', "
            "($1->0->'Physical Plans'->0->>'Candidate K')::bigint, "
            "$1->0->'Physical Plans'->0->'Estimated Work'", &view, 1, result));
        CHECK(uqa_value_destroy(&snapshot));
        CHECK(uqa_result_get_value(result, 0, 0, &view));
        CHECK(uqa_value_get_text(view, &text));
        assert(equal(text, "embedding"));
        CHECK(uqa_result_get_value(result, 0, 1, &view));
        CHECK(uqa_value_get_text(view, &text));
        assert(equal(text, "approximate"));
        assert(integer(result, 2) == 2);
        CHECK(uqa_result_get_value(result, 0, 3, &view));
        CHECK(uqa_value_get_is_null(view, &is_null));
        assert(is_null == (costs == 0));
    }
    CHECK(uqa_engine_execute(engine, "DROP TABLE c_explain_vectors", result));
}

static void explained_rule_delivery(uqa_engine engine, uqa_result result) {
    uqa_statement statement = NULL;
    uqa_value count = NULL;
    uint32_t state = 0;
    CHECK(uqa_engine_execute(engine,
        "CREATE TEMP SEQUENCE c_rule_rows; CREATE TEMP SEQUENCE c_rule_after; "
        "CREATE TEMP TABLE c_rule_target(n bigint); "
        "CREATE FUNCTION pg_temp.c_rule_after() RETURNS trigger LANGUAGE plpgsql AS $$ "
        "BEGIN PERFORM nextval('c_rule_after'); RETURN NULL; END $$; "
        "CREATE TRIGGER c_rule_after AFTER INSERT ON c_rule_target FOR EACH STATEMENT EXECUTE FUNCTION pg_temp.c_rule_after(); "
        "CREATE TEMP VIEW c_rule_event AS SELECT n FROM c_rule_target; "
        "CREATE RULE c_rule_route AS ON INSERT TO c_rule_event DO INSTEAD "
        "INSERT INTO c_rule_target VALUES(NEW.n) RETURNING n", result));
    CHECK(uqa_statement_prepare(engine,
        "EXPLAIN (ANALYZE, SERIALIZE BINARY) INSERT INTO c_rule_event "
        "SELECT nextval('c_rule_rows') FROM generate_series(1,$1::bigint) RETURNING n,NULL::aclitem",
        &statement));
    CHECK(uqa_value_create_int64(8, &count));
    CHECK(uqa_statement_bind(statement, 1, count));
    CHECK(uqa_value_destroy(&count));
    CHECK(uqa_engine_execute(engine, "BEGIN; SAVEPOINT before_rule_output", result));
    for (int attempt = 1; attempt <= 2; ++attempt) {
        assert(uqa_statement_execute(statement, result) == UQA_ERROR_SQL);
        sqlstate("42883");
        CHECK(uqa_transaction_get_state(engine, &state));
        assert(state == UQA_TRANSACTION_FAILED);
        CHECK(uqa_engine_execute(engine, "ROLLBACK TO before_rule_output", result));
        CHECK(uqa_transaction_get_state(engine, &state));
        assert(state == UQA_TRANSACTION_ACTIVE);
        CHECK(uqa_engine_execute(engine,
            "SELECT currval('c_rule_rows'),count(*) FROM c_rule_target", result));
        assert(integer(result, 0) == attempt && integer(result, 1) == 0);
    }
    CHECK(uqa_engine_execute(engine, "SELECT nextval('c_rule_after')", result));
    assert(integer(result, 0) == 1);
    CHECK(uqa_engine_execute(engine, "ROLLBACK", result));
    CHECK(uqa_statement_finalize(&statement));
    CHECK(uqa_statement_prepare(engine,
        "EXPLAIN (ANALYZE, SERIALIZE BINARY) INSERT INTO c_rule_event "
        "VALUES(nextval('c_rule_rows')+$1::bigint),(nextval('c_rule_rows')+$1::bigint) RETURNING n,NULL::aclitem",
        &statement));
    CHECK(uqa_value_create_int64(0, &count));
    CHECK(uqa_statement_bind(statement, 1, count));
    CHECK(uqa_value_destroy(&count));
    CHECK(uqa_engine_execute(engine, "BEGIN; SAVEPOINT before_values_output", result));
    for (int attempt = 3; attempt <= 4; ++attempt) {
        assert(uqa_statement_execute(statement, result) == UQA_ERROR_SQL);
        sqlstate("42883");
        CHECK(uqa_transaction_get_state(engine, &state));
        assert(state == UQA_TRANSACTION_FAILED);
        CHECK(uqa_engine_execute(engine, "ROLLBACK TO before_values_output", result));
        CHECK(uqa_engine_execute(engine,
            "SELECT currval('c_rule_rows'),count(*) FROM c_rule_target", result));
        assert(integer(result, 0) == attempt && integer(result, 1) == 0);
    }
    CHECK(uqa_engine_execute(engine, "SELECT nextval('c_rule_after')", result));
    assert(integer(result, 0) == 2);
    CHECK(uqa_engine_execute(engine, "ROLLBACK", result));
    CHECK(uqa_statement_finalize(&statement));
    CHECK(uqa_engine_execute(engine,
        "DROP VIEW c_rule_event; DROP TABLE c_rule_target; "
        "DROP FUNCTION pg_temp.c_rule_after(); DROP SEQUENCE c_rule_rows; DROP SEQUENCE c_rule_after", result));
}

static void joined_mutation_candidates(uqa_engine engine, uqa_result result) {
    CHECK(uqa_engine_execute(engine,
        "CREATE TEMP SEQUENCE c_candidate_ticks; CREATE TEMP TABLE c_candidate_target(m bigint); "
        "INSERT INTO c_candidate_target VALUES(0); "
        "UPDATE c_candidate_target SET m=nextval('c_candidate_ticks') FROM generate_series(1,2) g(k); "
        "SELECT m,nextval('c_candidate_ticks') FROM c_candidate_target", result));
    assert(integer(result, 0) == 1 && integer(result, 1) == 3);
    assert(uqa_engine_execute(engine,
        "UPDATE c_candidate_target SET m=1/(k-2) FROM generate_series(1,2) g(k)", result) == UQA_ERROR_SQL);
    sqlstate("22012");
    CHECK(uqa_engine_execute(engine,
        "CREATE FUNCTION pg_temp.c_candidate_skip() RETURNS trigger LANGUAGE plpgsql AS $$ "
        "BEGIN IF NEW.m=1 THEN RETURN NULL; END IF; RETURN NEW; END $$; "
        "CREATE TRIGGER c_candidate_skip BEFORE UPDATE ON c_candidate_target FOR EACH ROW EXECUTE FUNCTION pg_temp.c_candidate_skip(); "
        "UPDATE c_candidate_target SET m=k FROM generate_series(1,2) g(k) RETURNING m", result));
    assert(integer(result, 0) == 2);
    CHECK(uqa_engine_execute(engine,
        "DROP TABLE c_candidate_target; DROP FUNCTION pg_temp.c_candidate_skip(); DROP SEQUENCE c_candidate_ticks", result));
    CHECK(uqa_engine_execute(engine,
        "CREATE TEMP SEQUENCE c_point_ticks; "
        "CREATE DOMAIN pg_temp.c_point_type AS bigint CHECK(nextval('c_point_ticks')>0); "
        "CREATE TEMP TABLE c_point_target(id integer PRIMARY KEY,value pg_temp.c_point_type); "
        "INSERT INTO c_point_target VALUES(1,0); ALTER SEQUENCE c_point_ticks RESTART WITH 1; "
        "UPDATE c_point_target SET value=7 WHERE id=999; SELECT nextval('c_point_ticks')", result));
    assert(integer(result, 0) == 1);
    CHECK(uqa_engine_execute(engine,
        "ALTER TABLE c_point_target ADD CHECK(value>=0); ALTER SEQUENCE c_point_ticks RESTART WITH 1; "
        "UPDATE c_point_target SET value=7 WHERE id=1; SELECT nextval('c_point_ticks')", result));
    assert(integer(result, 0) == 2);
    CHECK(uqa_engine_execute(engine,
        "DROP TABLE c_point_target; DROP DOMAIN pg_temp.c_point_type; DROP SEQUENCE c_point_ticks", result));
}

static void explained_mutation_rule_delivery(uqa_engine engine, uqa_result result) {
    const char *queries[] = {
        "UPDATE c_mutation_event SET n=nextval('c_mutation_rows')+$1::bigint RETURNING n,NULL::aclitem",
        "DELETE FROM c_mutation_event WHERE nextval('c_mutation_rows')>$1::bigint RETURNING n,NULL::aclitem",
        "UPDATE c_mutation_target SET n=c_mutation_target.n FROM generate_series(1,2) g(k) WHERE nextval('c_mutation_rows')>$1::bigint RETURNING NULL::aclitem",
        "DELETE FROM c_mutation_target USING generate_series(1,2) g(k) WHERE nextval('c_mutation_rows')>$1::bigint RETURNING NULL::aclitem"
    };
    uqa_value value = NULL;
    uint32_t state = 0;
    CHECK(uqa_engine_execute(engine,
        "CREATE TEMP SEQUENCE c_mutation_rows; CREATE TEMP SEQUENCE c_mutation_after; "
        "CREATE TEMP TABLE c_mutation_target(n bigint); INSERT INTO c_mutation_target SELECT n FROM generate_series(1,8) g(n); "
        "CREATE FUNCTION pg_temp.c_mutation_after() RETURNS trigger LANGUAGE plpgsql AS $$ "
        "BEGIN PERFORM nextval('c_mutation_after'); RETURN NULL; END $$; "
        "CREATE TRIGGER c_mutation_after AFTER UPDATE OR DELETE ON c_mutation_target FOR EACH STATEMENT EXECUTE FUNCTION pg_temp.c_mutation_after(); "
        "CREATE TEMP VIEW c_mutation_event AS SELECT n FROM c_mutation_target; "
        "CREATE RULE c_mutation_update AS ON UPDATE TO c_mutation_event DO INSTEAD UPDATE c_mutation_target SET n=NEW.n WHERE n=OLD.n RETURNING n; "
        "CREATE RULE c_mutation_delete AS ON DELETE TO c_mutation_event DO INSTEAD DELETE FROM c_mutation_target WHERE n=OLD.n RETURNING n", result));
    CHECK(uqa_value_create_int64(0, &value));
    for (int query = 0; query < 4; ++query) {
        char sql[512];
        uqa_statement statement = NULL;
        int size = snprintf(sql, sizeof(sql), "EXPLAIN (ANALYZE, SERIALIZE BINARY) %s", queries[query]);
        assert(size > 0 && (size_t)size < sizeof(sql));
        CHECK(uqa_statement_prepare(engine, sql, &statement));
        CHECK(uqa_statement_bind(statement, 1, value));
        CHECK(uqa_engine_execute(engine, "BEGIN; SAVEPOINT before_mutation_output", result));
        for (int attempt = 1; attempt <= 2; ++attempt) {
            assert(uqa_statement_execute(statement, result) == UQA_ERROR_SQL);
            sqlstate("42883");
            CHECK(uqa_transaction_get_state(engine, &state));
            assert(state == UQA_TRANSACTION_FAILED);
            CHECK(uqa_engine_execute(engine, "ROLLBACK TO before_mutation_output", result));
            CHECK(uqa_transaction_get_state(engine, &state));
            assert(state == UQA_TRANSACTION_ACTIVE);
            CHECK(uqa_engine_execute(engine,
                "SELECT currval('c_mutation_rows'),count(*) FROM c_mutation_target", result));
            assert(integer(result, 0) == query * 2 + attempt && integer(result, 1) == 8);
        }
        CHECK(uqa_engine_execute(engine, "SELECT nextval('c_mutation_after')", result));
        assert(integer(result, 0) == query + 1);
        CHECK(uqa_engine_execute(engine, "ROLLBACK", result));
        CHECK(uqa_statement_finalize(&statement));
    }
    CHECK(uqa_value_destroy(&value));
    CHECK(uqa_engine_execute(engine,
        "DROP VIEW c_mutation_event; DROP TABLE c_mutation_target; "
        "DROP FUNCTION pg_temp.c_mutation_after(); DROP SEQUENCE c_mutation_rows; DROP SEQUENCE c_mutation_after", result));
}

static void explained_generic_parameters(uqa_engine engine, uqa_result result) {
    uqa_statement statement = NULL;
    uqa_value value = NULL;
    uqa_type type = NULL;
    uint32_t count = 0;
    uint32_t kind = 0;
    const char *integer_type[] = {"integer"};
    const char *unknown_type[] = {"unknown"};
    CHECK(uqa_engine_execute(engine, "EXPLAIN (GENERIC_PLAN, FORMAT JSON) SELECT $2::integer", result));
    CHECK(uqa_engine_execute(engine, "EXPLAIN (GENERIC_PLAN, FORMAT JSON) SELECT $1 IS NULL", result));
    assert(uqa_statement_prepare(engine, "EXPLAIN (GENERIC_PLAN) SELECT $2::integer", &statement) == UQA_ERROR_SQL);
    assert(statement == NULL);
    sqlstate("42P18");
    CHECK(uqa_statement_prepare_typed(engine, "EXPLAIN (GENERIC_PLAN, FORMAT JSON) SELECT $2::integer", integer_type, 1, &statement));
    CHECK(uqa_statement_get_parameter_count(statement, &count));
    assert(count == 2);
    CHECK(uqa_statement_get_parameter_type(statement, 2, &type));
    CHECK(uqa_type_get_kind(type, &kind));
    assert(kind == UQA_TYPE_INT32);
    CHECK(uqa_value_create_int64(7, &value));
    CHECK(uqa_statement_bind(statement, 1, value));
    CHECK(uqa_statement_bind(statement, 2, value));
    CHECK(uqa_value_destroy(&value));
    CHECK(uqa_statement_execute(statement, result));
    CHECK(uqa_statement_finalize(&statement));
    CHECK(uqa_statement_prepare_typed(engine, "", unknown_type, 1, &statement));
    CHECK(uqa_statement_get_parameter_count(statement, &count));
    assert(count == 1);
    assert(uqa_statement_get_parameter_type(statement, 1, &type) == UQA_DONE);
    assert(type == NULL);
    CHECK(uqa_value_create_text("unused", &value));
    CHECK(uqa_statement_bind(statement, 1, value));
    CHECK(uqa_value_destroy(&value));
    CHECK(uqa_statement_execute(statement, result));
    empty_result(result);
    CHECK(uqa_statement_finalize(&statement));
    CHECK(uqa_engine_execute(engine, "BEGIN; SAVEPOINT generic_retry", result));
    assert(uqa_engine_execute(engine, "EXPLAIN (GENERIC_PLAN) SELECT $1 + $2", result) == UQA_ERROR_SQL);
    sqlstate("42725");
    CHECK(uqa_engine_execute(engine, "ROLLBACK TO generic_retry", result));
    CHECK(uqa_engine_execute(engine, "EXPLAIN (GENERIC_PLAN) SELECT $1 IS NULL", result));
    CHECK(uqa_engine_execute(engine, "COMMIT", result));
}

static void explained_read_only_destination(uqa_engine engine, uqa_result result) {
    uqa_statement statement = NULL;
    uqa_value_view value = NULL;
    uqa_string_view text = {NULL, 0};
    CHECK(uqa_engine_execute(engine, "BEGIN READ ONLY", result));
    CHECK(uqa_statement_prepare(engine,
        "EXPLAIN ANALYZE CREATE TABLE c_explained_destination AS SELECT 1 AS id", &statement));
    CHECK(uqa_statement_execute(statement, result));
    CHECK(uqa_statement_finalize(&statement));
    CHECK(uqa_engine_execute(engine, "SHOW transaction_read_only", result));
    CHECK(uqa_result_get_value(result, 0, 0, &value));
    CHECK(uqa_value_get_text(value, &text));
    assert(equal(text, "on"));
    CHECK(uqa_engine_execute(engine, "SAVEPOINT protected_destination", result));
    assert(uqa_engine_execute(engine, "INSERT INTO c_explained_destination VALUES (2)", result) == UQA_ERROR_SQL);
    sqlstate("25006");
    CHECK(uqa_engine_execute(engine, "ROLLBACK TO protected_destination; COMMIT", result));
    CHECK(uqa_engine_execute(engine, "SELECT count(*) FROM c_explained_destination", result));
    assert(integer(result, 0) == 1);
}

static void utility_statements(uqa_engine engine, uqa_result result) {
    uqa_statement statement = NULL;
    uqa_type type = NULL;
    uqa_value value = NULL;
    uqa_value_view view = NULL;
    uqa_string_view text = {NULL, 0};
    uint32_t count = 0;
    uint32_t kind = 0;
    const char *types[] = {"integer"};

    CHECK(uqa_engine_execute(engine, "SET TIME ZONE 'UTC'", result));
    CHECK(uqa_statement_prepare(engine, "SHOW TIME ZONE", &statement));
    CHECK(uqa_statement_get_column_count(statement, &count));
    assert(count == 1);
    CHECK(uqa_statement_get_column_name(statement, 0, &text));
    assert(equal(text, "TimeZone"));
    CHECK(uqa_statement_get_column_type(statement, 0, &type));
    CHECK(uqa_type_get_kind(type, &kind));
    assert(kind == UQA_TYPE_TEXT);
    CHECK(uqa_statement_execute(statement, result));
    CHECK(uqa_result_get_value(result, 0, 0, &view));
    CHECK(uqa_value_get_text(view, &text));
    assert(equal(text, "UTC"));
    CHECK(uqa_statement_finalize(&statement));
    CHECK(uqa_statement_prepare(engine, "SHOW ALL", &statement));
    CHECK(uqa_statement_get_column_count(statement, &count));
    assert(count == 3);
    CHECK(uqa_statement_get_column_name(statement, 2, &text));
    assert(equal(text, "description"));
    CHECK(uqa_statement_finalize(&statement));

    CHECK(uqa_engine_execute(engine, "SET plan_cache_mode = force_custom_plan", result));
    CHECK(uqa_statement_prepare_typed(engine, "SHOW work_mem", types, 1, &statement));
    CHECK(uqa_value_create_int64(4, &value));
    CHECK(uqa_statement_bind(statement, 1, value));
    CHECK(uqa_value_destroy(&value));
    CHECK(uqa_statement_execute(statement, result));
    CHECK(uqa_engine_execute(engine,
        "SELECT generic_plans,custom_plans FROM pg_prepared_statements WHERE statement='SHOW work_mem'", result));
    assert(integer(result, 0) == 1 && integer(result, 1) == 0);
    CHECK(uqa_statement_finalize(&statement));

    CHECK(uqa_engine_execute(engine, "BEGIN", result));
    CHECK(uqa_statement_prepare(engine, "SHOW work_mem", &statement));
    CHECK(uqa_statement_finalize(&statement));
    CHECK(uqa_statement_prepare(engine, "FETCH ALL FROM absent", &statement));
    CHECK(uqa_statement_get_column_count(statement, &count));
    assert(count == 0);
    CHECK(uqa_statement_finalize(&statement));
    CHECK(uqa_engine_execute(engine, "SET TRANSACTION ISOLATION LEVEL SERIALIZABLE", result));
    CHECK(uqa_engine_execute(engine, "ROLLBACK", result));

    CHECK(uqa_engine_execute(engine, "BEGIN; DECLARE utility_cursor CURSOR FOR SELECT 1::integer AS old", result));
    CHECK(uqa_statement_prepare(engine, "FETCH ALL FROM utility_cursor", &statement));
    CHECK(uqa_statement_get_column_name(statement, 0, &text));
    assert(equal(text, "old"));
    CHECK(uqa_engine_execute(engine,
        "CLOSE utility_cursor; DECLARE utility_cursor CURSOR FOR SELECT 'new'::text AS different", result));
    CHECK(uqa_statement_execute(statement, result));
    CHECK(uqa_result_get_column_name(result, 0, &text));
    assert(equal(text, "different"));
    CHECK(uqa_result_get_value(result, 0, 0, &view));
    CHECK(uqa_value_get_text(view, &text));
    assert(equal(text, "new"));
    CHECK(uqa_engine_execute(engine, "DISCARD PLANS", result));
    CHECK(uqa_statement_execute(statement, result));
    CHECK(uqa_statement_get_column_name(statement, 0, &text));
    assert(equal(text, "old"));
    CHECK(uqa_statement_get_column_type(statement, 0, &type));
    CHECK(uqa_type_get_kind(type, &kind));
    assert(kind == UQA_TYPE_INT32);
    CHECK(uqa_statement_finalize(&statement));

    CHECK(uqa_statement_prepare(engine, "FETCH ALL FROM late_cursor", &statement));
    CHECK(uqa_statement_get_column_count(statement, &count));
    assert(count == 0);
    CHECK(uqa_engine_execute(engine, "DECLARE late_cursor CURSOR FOR SELECT 9::integer AS number", result));
    CHECK(uqa_statement_execute(statement, result));
    assert(integer(result, 0) == 9);
    CHECK(uqa_statement_get_column_count(statement, &count));
    assert(count == 0);
    CHECK(uqa_statement_finalize(&statement));
    CHECK(uqa_statement_prepare(engine, "MOVE ALL FROM late_cursor", &statement));
    CHECK(uqa_statement_get_column_count(statement, &count));
    assert(count == 0);
    CHECK(uqa_statement_execute(statement, result));
    CHECK(uqa_statement_finalize(&statement));
    CHECK(uqa_engine_execute(engine, "COMMIT", result));

    CHECK(uqa_engine_execute(engine, "PREPARE utility_query AS SELECT 1::integer AS old", result));
    CHECK(uqa_statement_prepare(engine, "EXECUTE utility_query", &statement));
    CHECK(uqa_statement_get_column_name(statement, 0, &text));
    assert(equal(text, "old"));
    CHECK(uqa_engine_execute(engine,
        "DEALLOCATE utility_query; PREPARE utility_query AS SELECT 'new'::text AS different", result));
    CHECK(uqa_statement_execute(statement, result));
    CHECK(uqa_result_get_column_name(result, 0, &text));
    assert(equal(text, "different"));
    CHECK(uqa_statement_get_column_name(statement, 0, &text));
    assert(equal(text, "old"));
    CHECK(uqa_statement_finalize(&statement));
    CHECK(uqa_engine_execute(engine, "DEALLOCATE utility_query", result));
    CHECK(uqa_engine_execute(engine, "PREPARE utility_parameter(integer) AS SELECT $1 AS value", result));
    CHECK(uqa_statement_prepare_typed(engine, "EXECUTE utility_parameter($1)", types, 1, &statement));
    CHECK(uqa_value_create_int64(4, &value));
    CHECK(uqa_statement_bind(statement, 1, value));
    CHECK(uqa_value_destroy(&value));
    assert(uqa_statement_execute(statement, result) == UQA_ERROR_SQL);
    sqlstate("42P02");
    CHECK(uqa_statement_finalize(&statement));
    CHECK(uqa_statement_prepare_typed(engine, "EXECUTE utility_parameter(4)", types, 1, &statement));
    CHECK(uqa_value_create_int64(5, &value));
    CHECK(uqa_statement_bind(statement, 1, value));
    CHECK(uqa_value_destroy(&value));
    CHECK(uqa_statement_execute(statement, result));
    assert(integer(result, 0) == 4);
    CHECK(uqa_statement_finalize(&statement));
    CHECK(uqa_engine_execute(engine, "DEALLOCATE utility_parameter", result));
    assert(uqa_statement_prepare(engine, "SHOW missing_prepared_setting", &statement) == UQA_ERROR_SQL);
    assert(statement == NULL);
    sqlstate("42704");
}

static void temporary_catalog_cleanup(void) {
    const char *directory = getenv("UQA_TEST_DATABASE_DIR");
    char path[4096];
    uqa_engine engine = NULL;
    uqa_result result = NULL;
    int size = snprintf(path, sizeof(path), "%s/temporary-cleanup", directory);
    assert(size > 0 && (size_t)size < sizeof(path));
    CHECK(test_database_open_path(path, NULL, &engine));
    CHECK(uqa_result_create(&result));
    CHECK(uqa_engine_execute(engine,
        "CREATE DOMAIN pg_temp.c_cleanup_domain AS integer; "
        "CREATE TABLE c_cleanup_table(n integer,d pg_temp.c_cleanup_domain); "
        "INSERT INTO c_cleanup_table VALUES(7,1); "
        "CREATE VIEW c_cleanup_view AS SELECT d FROM c_cleanup_table", result));
    CHECK(uqa_engine_execute(engine, "BEGIN READ ONLY; DISCARD TEMP", result));
    CHECK(uqa_engine_execute(engine,
        "SELECT count(*) FROM pg_attribute WHERE attrelid='c_cleanup_table'::regclass "
        "AND attnum>0 AND NOT attisdropped", result));
    assert(integer(result, 0) == 1);
    CHECK(uqa_engine_execute(engine, "ROLLBACK", result));
    CHECK(uqa_engine_execute(engine, "SELECT d FROM c_cleanup_table", result));
    assert(integer(result, 0) == 1);
    CHECK(uqa_result_destroy(&result));
    CHECK(uqa_engine_close(&engine));
    CHECK(uqa_engine_close(&engine));
    if (test_database_flags() != UQA_OPEN_MEMORY) {
        CHECK(test_database_open_path(path, NULL, &engine));
        CHECK(uqa_result_create(&result));
        CHECK(uqa_engine_execute(engine,
            "SELECT n,(SELECT count(*) FROM pg_type WHERE typname='c_cleanup_domain'),"
            "(SELECT count(*) FROM pg_class WHERE relname='c_cleanup_view') FROM c_cleanup_table", result));
        assert(integer(result, 0) == 7 && integer(result, 1) == 0 && integer(result, 2) == 0);
        CHECK(uqa_engine_execute(engine,
            "SELECT count(*) FROM pg_attribute WHERE attrelid='c_cleanup_table'::regclass "
            "AND attnum>0 AND NOT attisdropped", result));
        assert(integer(result, 0) == 1);
        CHECK(uqa_result_destroy(&result));
        CHECK(uqa_engine_close(&engine));
    }
}

static void temporary_catalog_timeout(void) {
    if (test_database_flags() == UQA_OPEN_MEMORY) {
        return;
    }
    const char *directory = getenv("UQA_TEST_DATABASE_DIR");
    char path[4096];
    uqa_engine engine = NULL;
    uqa_engine peer = NULL;
    uqa_result result = NULL;
    int size = snprintf(path, sizeof(path), "%s/temporary-timeout", directory);
    assert(size > 0 && (size_t)size < sizeof(path));
    CHECK(test_database_open_path(path, NULL, &engine));
    CHECK(uqa_engine_open_session(engine, &peer));
    CHECK(uqa_result_create(&result));
    CHECK(uqa_engine_execute(engine,
        "CREATE DOMAIN pg_temp.c_timeout_domain AS integer; "
        "CREATE TABLE c_timeout_table(n integer,d pg_temp.c_timeout_domain); "
        "INSERT INTO c_timeout_table VALUES(7,1); "
        "CREATE VIEW c_timeout_view AS SELECT d FROM c_timeout_table", result));
    CHECK(uqa_engine_execute(engine, "SET idle_session_timeout='100ms'", result));
    for (unsigned attempt = 0;; ++attempt) {
        CHECK(uqa_engine_execute(peer,
            "SELECT count(*) FROM pg_type WHERE typname='c_timeout_domain'", result));
        if (integer(result, 0) == 0) {
            break;
        }
        assert(attempt < 2000);
        CHECK(uqa_engine_execute(peer, "SELECT pg_sleep(0.01)", result));
    }
    CHECK(uqa_engine_execute(peer,
        "SELECT n,(SELECT count(*) FROM pg_class WHERE relname='c_timeout_view'),"
        "(SELECT count(*) FROM pg_attribute WHERE attrelid='c_timeout_table'::regclass "
        "AND attnum>0 AND NOT attisdropped) FROM c_timeout_table", result));
    assert(integer(result, 0) == 7 && integer(result, 1) == 0 && integer(result, 2) == 1);
    assert(uqa_engine_execute(engine, "SELECT 1", result) == UQA_ERROR_SQL);
    sqlstate("57P05");
    CHECK(uqa_result_destroy(&result));
    CHECK(uqa_engine_close(&engine));
    CHECK(uqa_engine_close(&peer));
    CHECK(test_database_open_path(path, NULL, &engine));
    CHECK(uqa_result_create(&result));
    CHECK(uqa_engine_execute(engine,
        "SELECT n,(SELECT count(*) FROM pg_type WHERE typname='c_timeout_domain') FROM c_timeout_table", result));
    assert(integer(result, 0) == 7 && integer(result, 1) == 0);
    CHECK(uqa_result_destroy(&result));
    CHECK(uqa_engine_close(&engine));
}

int main(void) {
    uqa_engine engine = NULL;
    uqa_result result = NULL;
    uqa_statement statement = NULL;
    uqa_value value = NULL;
    uqa_value_view view = NULL;
    uqa_type type = NULL;
    uqa_string_view name = {NULL, 0};
    uint32_t count = 99;
    uint32_t kind = 0;
    uint8_t is_null = 0;
    uint64_t rows = 99;

    CHECK(test_database_open(NULL, &engine));
    CHECK(uqa_result_create(&result));
    CHECK(uqa_engine_execute(engine, "SELECT 81", result));
    CHECK(uqa_statement_prepare(engine, "SELECT $1::integer AS x, $1 + 1 AS x", &statement));
    CHECK(uqa_statement_get_parameter_count(statement, &count));
    assert(count == 1);
    CHECK(uqa_statement_get_parameter_type(statement, 1, &type));
    CHECK(uqa_type_get_kind(type, &kind));
    assert(kind == UQA_TYPE_INT32);
    CHECK(uqa_statement_get_column_count(statement, &count));
    assert(count == 2);
    CHECK(uqa_statement_get_column_name(statement, 0, &name));
    assert(equal(name, "x"));
    CHECK(uqa_statement_get_column_name(statement, 1, &name));
    assert(equal(name, "x"));
    CHECK(uqa_statement_get_column_type(statement, 0, &type));
    CHECK(uqa_type_get_kind(type, &kind));
    assert(kind == UQA_TYPE_INT32);
    assert(uqa_statement_get_parameter_type(statement, 0, &type) == UQA_ERROR_RANGE);
    assert(type == NULL);
    assert(uqa_statement_execute(statement, result) == UQA_ERROR_STATE);
    assert(integer(result, 0) == 81);
    CHECK(uqa_value_create_int64(7, &value));
    CHECK(uqa_statement_bind(statement, 1, value));
    CHECK(uqa_value_destroy(&value));
    assert(uqa_statement_bind(statement, 1, NULL) == UQA_ERROR_INVALID_ARGUMENT);
    CHECK(uqa_statement_execute(statement, result));
    assert(integer(result, 0) == 7 && integer(result, 1) == 8);
    CHECK(uqa_statement_execute(statement, result));
    assert(integer(result, 0) == 7);
    CHECK(uqa_statement_clear_bindings(statement));
    assert(uqa_statement_execute(statement, result) == UQA_ERROR_STATE);
    assert(integer(result, 0) == 7);
    CHECK(uqa_value_create_null(&value));
    CHECK(uqa_statement_bind(statement, 1, value));
    CHECK(uqa_value_destroy(&value));
    CHECK(uqa_statement_execute(statement, result));
    CHECK(uqa_result_get_value(result, 0, 0, &view));
    CHECK(uqa_value_get_is_null(view, &is_null));
    assert(is_null);
    assert(uqa_engine_close(&engine) == UQA_ERROR_BUSY);
    CHECK(uqa_statement_finalize(&statement));
    CHECK(uqa_value_get_is_null(view, &is_null));
    assert(is_null);

    /* Explicit declarations preserve native typing even for a NULL value. */
    {
        const char *types[] = {"bigint"};
        CHECK(uqa_statement_prepare_typed(engine, "SELECT $1 AS n", types, 1, &statement));
        CHECK(uqa_statement_get_parameter_type(statement, 1, &type));
        CHECK(uqa_type_get_kind(type, &kind));
        assert(kind == UQA_TYPE_INT64);
        CHECK(uqa_value_create_null(&value));
        CHECK(uqa_statement_bind(statement, 1, value));
        CHECK(uqa_value_destroy(&value));
        CHECK(uqa_statement_execute(statement, result));
        CHECK(uqa_result_get_column_type(result, 0, &type));
        CHECK(uqa_type_get_kind(type, &kind));
        assert(kind == UQA_TYPE_INT64);
        CHECK(uqa_statement_finalize(&statement));
    }
    {
        const char query[] = "SELECT $1::integer WHERE false trailing";
        const char type_name[] = {'i', 'n', 't'};
        const char *types[] = {type_name};
        const size_t sizes[] = {sizeof(type_name)};
        CHECK(uqa_statement_prepare_typed_n(engine, query, strlen("SELECT $1::integer WHERE false"),
                                             types, sizes, 1, &statement));
        CHECK(uqa_value_create_int64(3, &value));
        CHECK(uqa_statement_bind(statement, 1, value));
        CHECK(uqa_value_destroy(&value));
        CHECK(uqa_statement_execute(statement, result));
        CHECK(uqa_result_get_row_count(result, &rows));
        assert(rows == 0);
        CHECK(uqa_statement_get_column_count(statement, &count));
        assert(count == 1);
        CHECK(uqa_statement_finalize(&statement));
    }

    CHECK(uqa_engine_execute(engine, "CREATE TABLE items (n integer)", result));
    CHECK(uqa_statement_prepare_n(engine, "INSERT INTO items VALUES ($1) RETURNING n",
                                  strlen("INSERT INTO items VALUES ($1) RETURNING n"), &statement));
    CHECK(uqa_value_create_int64(9, &value));
    CHECK(uqa_statement_bind(statement, 1, value));
    CHECK(uqa_value_destroy(&value));
    CHECK(uqa_engine_execute(engine, "BEGIN", result));
    CHECK(uqa_statement_execute(statement, result));
    assert(integer(result, 0) == 9);
    CHECK(uqa_engine_execute(engine, "ROLLBACK", result));
    CHECK(uqa_statement_execute(statement, result));
    assert(integer(result, 0) == 9);
    CHECK(uqa_statement_finalize(&statement));
    CHECK(uqa_engine_execute(engine, "SELECT count(*) FROM items", result));
    assert(integer(result, 0) == 1);

    CHECK(uqa_statement_prepare(engine, "SELECT * FROM items", &statement));
    CHECK(uqa_engine_execute(engine, "ALTER TABLE items ADD COLUMN extra text", result));
    assert(uqa_statement_execute(statement, result) == UQA_ERROR_SQL);
    sqlstate("0A000");
    CHECK(uqa_statement_finalize(&statement));

    /* A SQL user can discover a host definition's name, but replacing that name
       cannot make an older C handle target the new definition. */
    CHECK(uqa_statement_prepare(engine, "SELECT 33 AS saved", &statement));
    CHECK(uqa_engine_execute(engine, "SELECT name FROM pg_prepared_statements", result));
    CHECK(uqa_result_get_value(result, 0, 0, &view));
    CHECK(uqa_value_get_text(view, &name));
    {
        char commands[256];
        int size = snprintf(commands, sizeof(commands), "DEALLOCATE \"%.*s\"; PREPARE \"%.*s\" AS SELECT 44",
                            (int)name.size, name.data, (int)name.size, name.data);
        assert(size > 0 && (size_t)size < sizeof(commands));
        CHECK(uqa_engine_execute(engine, commands, result));
    }
    assert(uqa_statement_get_parameter_count(statement, &count) == UQA_ERROR_SQL);
    assert(count == 0);
    sqlstate("26000");
    assert(uqa_statement_execute(statement, result) == UQA_ERROR_SQL);
    CHECK(uqa_statement_finalize(&statement));
    CHECK(uqa_engine_execute(engine, "SELECT count(*) FROM pg_prepared_statements", result));
    assert(integer(result, 0) == 1);
    CHECK(uqa_engine_execute(engine, "DEALLOCATE ALL", result));

    CHECK(uqa_statement_prepare(engine, "SELECT 55", &statement));
    CHECK(uqa_engine_execute(engine, "DISCARD ALL", result));
    assert(uqa_statement_execute(statement, result) == UQA_ERROR_SQL);
    sqlstate("26000");
    CHECK(uqa_statement_finalize(&statement));
    CHECK(uqa_statement_finalize(&statement));
    assert(uqa_statement_prepare(engine, "SELECT 1; SELECT 2", &statement) == UQA_ERROR_SQL);
    assert(statement == NULL);

    CHECK(uqa_engine_execute(engine, "BEGIN", result));
    CHECK(uqa_statement_prepare(engine, " ; -- empty\n; ", &statement));
    CHECK(uqa_statement_get_parameter_count(statement, &count));
    assert(count == 0);
    CHECK(uqa_statement_get_column_count(statement, &count));
    assert(count == 0);
    CHECK(uqa_statement_execute(statement, result));
    empty_result(result);
    CHECK(uqa_engine_execute(engine, "SET TRANSACTION ISOLATION LEVEL SERIALIZABLE", result));
    CHECK(uqa_engine_execute(engine, "SAVEPOINT keep", result));
    assert(uqa_engine_execute(engine, "SELECT 1/0", result) == UQA_ERROR_SQL);
    sqlstate("22012");
    assert(uqa_statement_execute(statement, result) == UQA_ERROR_SQL);
    sqlstate("25P02");
    CHECK(uqa_statement_finalize(&statement));
    CHECK(uqa_statement_prepare_n(engine, "ignored", 0, &statement));
    CHECK(uqa_statement_get_column_count(statement, &count));
    assert(count == 0);
    assert(uqa_statement_execute(statement, result) == UQA_ERROR_SQL);
    sqlstate("25P02");
    CHECK(uqa_engine_execute(engine, "ROLLBACK TO keep", result));
    CHECK(uqa_statement_execute(statement, result));
    empty_result(result);
    CHECK(uqa_statement_finalize(&statement));
    CHECK(uqa_engine_execute(engine, "COMMIT", result));

    {
        const char *types[] = {"integer"};
        CHECK(uqa_statement_prepare_typed(engine, "", types, 1, &statement));
        CHECK(uqa_statement_get_parameter_type(statement, 1, &type));
        CHECK(uqa_type_get_kind(type, &kind));
        assert(kind == UQA_TYPE_INT32);
        assert(uqa_statement_execute(statement, result) == UQA_ERROR_STATE);
        CHECK(uqa_value_create_int64(4, &value));
        CHECK(uqa_statement_bind(statement, 1, value));
        CHECK(uqa_value_destroy(&value));
        CHECK(uqa_statement_execute(statement, result));
        empty_result(result);
        CHECK(uqa_statement_finalize(&statement));
    }
    utility_statements(engine, result);
    prepared_recovery(engine, result);
    procedure_statements(engine, result);
    explained_inputs(engine, result);
    explained_results(engine, result);
    explained_stream_and_settings(engine, result);
    explained_physical_metadata(engine, result);
    explained_costs(engine, result);
    explained_named_inputs(engine, result);
    explained_serialization(engine, result);
    explained_rule_delivery(engine, result);
    explained_mutation_rule_delivery(engine, result);
    joined_mutation_candidates(engine, result);
    explained_generic_parameters(engine, result);
    explained_read_only_destination(engine, result);
    xml_values(engine, result);
    CHECK(uqa_result_destroy(&result));
    CHECK(uqa_engine_close(&engine));
    temporary_catalog_cleanup();
    temporary_catalog_timeout();
    return 0;
}
