/*
 * Unified Query Algebra
 *
 * Copyright (c) 2023-2026 Cognica, Inc.
 */

#include <uqa/uqa.h>
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

    CHECK(uqa_engine_open_memory(NULL, &engine));
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
    xml_values(engine, result);
    CHECK(uqa_result_destroy(&result));
    CHECK(uqa_engine_close(&engine));
    return 0;
}
