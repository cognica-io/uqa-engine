//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 `to_regproc`, `to_regprocedure`, `to_regclass`, `to_regnamespace`, `to_regrole`, and `to_regtype` parity.

use super::*;

fn create_lookup_objects(eng: &Engine) {
    for sql in [
        "CREATE SCHEMA reg_lookup",
        "CREATE SCHEMA \"select\"",
        "CREATE TABLE reg_lookup.\"MixedTable\" (id INTEGER)",
        "CREATE TABLE reg_lookup.\"a-b\" (id INTEGER)",
        "CREATE FUNCTION reg_lookup.one(value INTEGER) RETURNS INTEGER LANGUAGE SQL IMMUTABLE AS 'SELECT value'",
        "CREATE FUNCTION reg_lookup.\"select\"(value INTEGER) RETURNS INTEGER LANGUAGE SQL IMMUTABLE AS 'SELECT value'",
        "CREATE FUNCTION reg_lookup.\"paren(integer)\"(value INTEGER) RETURNS INTEGER LANGUAGE SQL IMMUTABLE AS 'SELECT value'",
        "CREATE FUNCTION reg_lookup.overloaded(value INTEGER) RETURNS INTEGER LANGUAGE SQL IMMUTABLE AS 'SELECT value'",
        "CREATE FUNCTION reg_lookup.overloaded(value TEXT) RETURNS TEXT LANGUAGE SQL IMMUTABLE AS 'SELECT value'",
        "CREATE PROCEDURE reg_lookup.proc(value INTEGER) LANGUAGE SQL AS 'SELECT value'",
        "SET search_path = reg_lookup, public",
    ] {
        eng.sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
}

fn assert_relation_lookups(eng: &Engine) {
    let table_oid = scalar(
        eng,
        "SELECT oid FROM pg_catalog.pg_class WHERE relname = 'MixedTable' AND relnamespace = (SELECT oid FROM pg_catalog.pg_namespace WHERE nspname = 'reg_lookup')",
    );
    assert_eq!(
        scalar(eng, "SELECT to_regclass('\"MixedTable\"')::oid"),
        table_oid
    );
    assert_eq!(
        text(eng, "SELECT to_regclass('\"MixedTable\"')::text"),
        "\"MixedTable\""
    );
    assert_eq!(
        scalar(eng, "SELECT to_regclass('pg_catalog.pg_type')::oid"),
        Value::Int(1247)
    );
    assert_eq!(text(eng, "SELECT to_regclass('a-b')::text"), "\"a-b\"");
}

fn assert_routine_lookups(eng: &Engine) {
    let function_oid = scalar(
        eng,
        "SELECT oid FROM pg_catalog.pg_proc WHERE proname = 'one' AND pronamespace = (SELECT oid FROM pg_catalog.pg_namespace WHERE nspname = 'reg_lookup')",
    );
    assert_eq!(scalar(eng, "SELECT to_regproc('one')::oid"), function_oid);
    assert_eq!(
        scalar(eng, "SELECT to_regprocedure('one(integer)')::oid"),
        function_oid
    );
    let procedure_oid = scalar(
        eng,
        "SELECT oid FROM pg_catalog.pg_proc WHERE proname = 'proc' AND pronamespace = (SELECT oid FROM pg_catalog.pg_namespace WHERE nspname = 'reg_lookup')",
    );
    assert_eq!(
        scalar(eng, "SELECT to_regprocedure('proc(integer)')::oid"),
        procedure_oid
    );
    assert_eq!(
        scalar(eng, "SELECT to_regproc('overloaded') IS NULL"),
        Value::Bool(true)
    );
    assert_eq!(
        scalar(eng, "SELECT to_regprocedure('one(text)') IS NULL"),
        Value::Bool(true)
    );
    assert_eq!(text(eng, "SELECT to_regproc('select')::text"), "\"select\"");
    assert_eq!(
        text(eng, "SELECT to_regprocedure('select(integer)')::text"),
        "\"select\"(integer)"
    );
    assert_eq!(
        text(eng, "SELECT to_regproc('paren(integer)')::text"),
        "\"paren(integer)\""
    );
    assert_eq!(
        scalar(eng, "SELECT to_regprocedure('paren(integer)') IS NULL"),
        Value::Bool(true)
    );
    assert_eq!(
        text(eng, "SELECT to_regprocedure('to_bin(integer)')::text"),
        "to_bin(integer)"
    );
    assert_eq!(
        scalar(eng, "SELECT to_regprocedure('to_bin(\"integer\")') IS NULL"),
        Value::Bool(true)
    );
}

fn assert_namespace_and_type_lookups(eng: &Engine) {
    let namespace_oid = scalar(
        eng,
        "SELECT oid FROM pg_catalog.pg_namespace WHERE nspname = 'reg_lookup'",
    );
    assert_eq!(
        scalar(eng, "SELECT to_regnamespace('reg_lookup')::oid"),
        namespace_oid
    );
    assert_eq!(
        scalar(eng, "SELECT 'reg_lookup'::regnamespace::oid"),
        namespace_oid
    );
    assert_eq!(
        scalar(
            eng,
            "SELECT relnamespace = current_schema()::regnamespace FROM pg_catalog.pg_class WHERE oid = 'reg_lookup.\"MixedTable\"'::regclass",
        ),
        Value::Bool(true)
    );
    assert_eq!(
        text(eng, "SELECT to_regnamespace('select')::text"),
        "\"select\""
    );
    for (input, oid, output) in [
        ("integer", 23, "integer"),
        ("pg_catalog.int4", 23, "integer"),
        ("integer[]", 1007, "integer[]"),
        ("integer[][]", 1007, "integer[]"),
        ("varchar(10)", 1043, "character varying"),
        (
            "information_schema.cardinal_number",
            13_307,
            "information_schema.cardinal_number",
        ),
    ] {
        assert_eq!(
            scalar(eng, &format!("SELECT to_regtype('{input}')::oid")),
            Value::Int(oid),
            "{input}"
        );
        assert_eq!(
            text(eng, &format!("SELECT to_regtype('{input}')::text")),
            output,
            "{input}"
        );
    }
    assert_eq!(
        scalar(eng, "SELECT to_regtype('\"integer\"') IS NULL"),
        Value::Bool(true)
    );
}

#[test]
fn pg18_regnamespace_casts_preserve_oid_identity_and_hard_errors() {
    let eng = engine();
    create_lookup_objects(&eng);
    assert_eq!(
        text(&eng, "SELECT 11::oid::regnamespace::text"),
        "pg_catalog"
    );
    for (sql, state, detail) in [
        (
            "SELECT 'missing_namespace'::regnamespace",
            "3F000",
            "schema \"missing_namespace\" does not exist",
        ),
        (
            "SELECT '09'::regnamespace",
            "22P02",
            "invalid input syntax for type oid",
        ),
        (
            "SELECT '4294967296'::regnamespace",
            "22003",
            "out of range for type oid",
        ),
        ("SELECT 'a.b'::regnamespace", "42602", "invalid name syntax"),
        ("SELECT ''::regnamespace", "42602", "invalid name syntax"),
    ] {
        let error = eng.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some(state), "{sql}: {error}");
        assert!(error.to_string().contains(detail), "{sql}: {error}");
    }
}

fn assert_lookup_result_types(eng: &Engine) {
    for (sql, expected) in [
        ("SELECT pg_typeof(to_regproc('one'))", "regproc"),
        (
            "SELECT pg_typeof(to_regprocedure('one(integer)'))",
            "regprocedure",
        ),
        ("SELECT pg_typeof(to_regclass('pg_type'))", "regclass"),
        (
            "SELECT pg_typeof(to_regnamespace('reg_lookup'))",
            "regnamespace",
        ),
        ("SELECT pg_typeof(to_regrole('uqa'))", "regrole"),
        ("SELECT pg_typeof(to_regtype('integer'))", "regtype"),
    ] {
        assert_eq!(text(eng, sql), expected, "{sql}");
    }
}

#[test]
fn pg18_to_reg_lookups_resolve_visible_catalog_objects_and_exact_types() {
    let eng = engine();
    create_lookup_objects(&eng);
    assert_relation_lookups(&eng);
    assert_routine_lookups(&eng);
    assert_namespace_and_type_lookups(&eng);
    assert_lookup_result_types(&eng);
}

fn create_regrole_fixtures(eng: &Engine) {
    for sql in [
        "CREATE ROLE reg_lookup_role",
        "CREATE ROLE \"Mixed-Role\"",
        "CREATE ROLE \"select\"",
    ] {
        eng.sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
}

#[test]
fn pg18_to_regrole_resolves_roles_oid_forms_casts_and_arrays() {
    let eng = engine();
    create_regrole_fixtures(&eng);
    let role_oid = scalar(
        &eng,
        "SELECT oid FROM pg_catalog.pg_roles WHERE rolname = 'reg_lookup_role'",
    );
    assert_eq!(
        scalar(&eng, "SELECT to_regrole('reg_lookup_role')::oid"),
        role_oid
    );
    assert_eq!(
        text(&eng, "SELECT to_regrole('REG_LOOKUP_ROLE')::text"),
        "reg_lookup_role"
    );
    assert_eq!(
        text(&eng, "SELECT to_regrole('\"Mixed-Role\"')::text"),
        "\"Mixed-Role\""
    );
    assert_eq!(
        text(&eng, "SELECT to_regrole('select')::text"),
        "\"select\""
    );
    assert_eq!(text(&eng, "SELECT pg_typeof(to_regrole('uqa'))"), "regrole");
    for (input, oid) in [("23", 23), ("00023", 19), ("-", 0)] {
        assert_eq!(
            scalar(&eng, &format!("SELECT to_regrole('{input}')::oid")),
            Value::Int(oid),
            "{input}"
        );
    }
    for input in ["missing", "09", "4294967296", "one.two", "\"unterminated"] {
        assert_eq!(
            scalar(&eng, &format!("SELECT to_regrole('{input}') IS NULL")),
            Value::Bool(true),
            "{input}"
        );
    }
    assert_eq!(scalar(&eng, "SELECT to_regrole(NULL)"), Value::Null);
    assert_eq!(
        scalar(&eng, "SELECT 'reg_lookup_role'::regrole::oid"),
        role_oid
    );
    assert_eq!(
        scalar(&eng, "SELECT ('{reg_lookup_role}'::regrole[])[1]::oid"),
        role_oid
    );
    assert_eq!(
        text(
            &eng,
            "SELECT ARRAY['reg_lookup_role'::regrole, '\"Mixed-Role\"'::regrole]::text",
        ),
        "{reg_lookup_role,\"\\\"Mixed-Role\\\"\"}"
    );
}

#[test]
fn pg18_regrole_casts_preserve_hard_errors_and_stable_volatility() {
    let eng = engine();
    for (sql, state, detail) in [
        (
            "SELECT 'missing'::regrole",
            "42704",
            "role \"missing\" does not exist",
        ),
        (
            "SELECT '09'::regrole",
            "22P02",
            "invalid input syntax for type oid",
        ),
        (
            "SELECT '4294967296'::regrole",
            "22003",
            "out of range for type oid",
        ),
        ("SELECT 'one.two'::regrole", "42602", "invalid name syntax"),
        (
            "SELECT '{missing}'::regrole[]",
            "42704",
            "role \"missing\" does not exist",
        ),
    ] {
        let error = eng.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some(state), "{sql}: {error}");
        assert!(error.to_string().contains(detail), "{sql}: {error}");
    }
    let error = eng
        .sql(
            "CREATE TABLE invalid_regrole_generation (name TEXT, owner regrole GENERATED ALWAYS AS (to_regrole(name)) STORED)",
            &[],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42P17"));
    assert!(error.to_string().contains("not immutable"), "{error}");
}

#[test]
fn pg18_regrole_constants_follow_stored_expression_dependency_rules() {
    let eng = engine();
    for sql in [
        "CREATE TABLE invalid_regrole_default (owner regrole DEFAULT 'uqa')",
        "CREATE TABLE invalid_regrole_check (owner regrole CHECK (owner <> 'uqa'::regrole))",
        "CREATE TABLE invalid_regrole_literal_generation (owner regrole GENERATED ALWAYS AS ('uqa') STORED)",
        "CREATE VIEW invalid_regrole_view AS SELECT 'uqa'::regrole AS owner",
        "CREATE MATERIALIZED VIEW invalid_regrole_materialized_view AS SELECT 'uqa'::regrole AS owner",
    ] {
        let error = eng.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("0A000"), "{sql}: {error}");
        assert!(
            error
                .to_string()
                .contains("constant of the type regrole cannot be used here"),
            "{sql}: {error}"
        );
    }
    for (sql, state, detail) in [
        (
            "CREATE TABLE invalid_regrole_default_missing (owner regrole DEFAULT 'missing_regrole')",
            "42704",
            "role \"missing_regrole\" does not exist",
        ),
        (
            "CREATE TABLE invalid_regrole_default_oid (owner regrole DEFAULT '09')",
            "22P02",
            "invalid input syntax for type oid",
        ),
        (
            "CREATE TABLE invalid_regrole_default_name (owner regrole DEFAULT 'one.two')",
            "42602",
            "invalid name syntax",
        ),
        (
            "CREATE VIEW invalid_regrole_view_missing AS SELECT 'missing_regrole'::regrole AS owner",
            "42704",
            "role \"missing_regrole\" does not exist",
        ),
        (
            "CREATE VIEW invalid_regrole_view_oid AS SELECT '09'::regrole AS owner",
            "22P02",
            "invalid input syntax for type oid",
        ),
        (
            "CREATE VIEW invalid_regrole_view_name AS SELECT 'one.two'::regrole AS owner",
            "42602",
            "invalid name syntax",
        ),
        (
            "CREATE TABLE invalid_regrole_check_missing (owner regrole CHECK (owner <> 'missing_regrole'::regrole))",
            "42704",
            "role \"missing_regrole\" does not exist",
        ),
        (
            "CREATE TABLE invalid_regrole_generation_missing (owner regrole GENERATED ALWAYS AS ('missing_regrole') STORED)",
            "42704",
            "role \"missing_regrole\" does not exist",
        ),
    ] {
        let error = eng.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some(state), "{sql}: {error}");
        assert!(error.to_string().contains(detail), "{sql}: {error}");
    }
    eng.sql("CREATE TABLE regrole_alter_constants (owner regrole)", &[])
        .unwrap();
    for sql in [
        "ALTER TABLE regrole_alter_constants ALTER COLUMN owner SET DEFAULT 'uqa'",
        "ALTER TABLE regrole_alter_constants ADD COLUMN backup regrole DEFAULT 'uqa'",
        "ALTER TABLE regrole_alter_constants ADD CONSTRAINT owner_not_uqa CHECK (owner <> 'uqa'::regrole)",
    ] {
        let error = eng.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("0A000"), "{sql}: {error}");
    }
}

#[test]
fn pg18_regrole_check_semantics_precede_dependency_rejection() {
    let eng = engine();
    for sql in [
        "CREATE TABLE invalid_regrole_check_column_left (owner regrole, CHECK (missing_column IS NULL AND owner <> '23'::regrole))",
        "CREATE TABLE invalid_regrole_check_column_right (owner regrole, CHECK (owner <> '23'::regrole AND missing_column IS NULL))",
        "CREATE TABLE invalid_regrole_partition_column (owner regrole) PARTITION BY HASH (((missing_column + ('23'::regrole)::oid)))",
    ] {
        let error = eng.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42703"), "{sql}: {error}");
        assert!(error.to_string().contains("missing_column"), "{sql}: {error}");
    }
    let sql = "CREATE TABLE invalid_regrole_check_type (owner regrole CHECK ('23'::regrole))";
    let error = eng.sql(sql, &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some("42804"), "{sql}: {error}");
    assert!(
        error
            .to_string()
            .contains("argument of CHECK must be type boolean, not type regrole"),
        "{sql}: {error}"
    );
    eng.sql(
        "CREATE TABLE regrole_check_alter_order (owner regrole)",
        &[],
    )
    .unwrap();
    let sql = "ALTER TABLE regrole_check_alter_order ADD CHECK (owner <> '23'::regrole AND missing_column IS NULL)";
    let error = eng.sql(sql, &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some("42703"), "{sql}: {error}");
}

#[test]
fn pg18_regrole_constants_are_rejected_across_catalog_objects() {
    let eng = engine();
    for sql in [
        "CREATE FUNCTION invalid_regrole_parameter_default(value regrole DEFAULT '23') RETURNS oid LANGUAGE SQL IMMUTABLE AS 'SELECT value::oid'",
        "CREATE PROCEDURE invalid_regrole_procedure_default(value regrole DEFAULT '23') LANGUAGE SQL AS 'SELECT value::oid'",
        "CREATE FUNCTION invalid_regrole_standard_body() RETURNS regrole LANGUAGE SQL IMMUTABLE RETURN '23'::regrole",
        "CREATE TABLE invalid_regrole_partition_key (owner regrole) PARTITION BY HASH (((owner::oid::bigint + ('23'::regrole)::oid::bigint)))",
    ] {
        let error = eng.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("0A000"), "{sql}: {error}");
    }
    for sql in [
        "CREATE TABLE regrole_event_source (owner regrole)",
        "CREATE TABLE regrole_event_sink (owner regrole)",
        "CREATE FUNCTION regrole_event_trigger() RETURNS trigger LANGUAGE plpgsql AS 'BEGIN RETURN NEW; END'",
    ] {
        eng.sql(sql, &[]).unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    for sql in [
        "CREATE TRIGGER invalid_regrole_trigger BEFORE INSERT ON regrole_event_source FOR EACH ROW WHEN (NEW.owner <> '23'::regrole) EXECUTE FUNCTION regrole_event_trigger()",
        "CREATE RULE invalid_regrole_rule_condition AS ON INSERT TO regrole_event_source WHERE NEW.owner <> '23'::regrole DO NOTHING",
        "CREATE RULE invalid_regrole_rule_action AS ON INSERT TO regrole_event_source DO ALSO INSERT INTO regrole_event_sink VALUES ('23'::regrole)",
    ] {
        let error = eng.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("0A000"), "{sql}: {error}");
    }
}

#[test]
fn pg18_regrole_routine_dependencies_preserve_error_precedence() {
    let eng = engine();
    for (sql, state, detail) in [
        (
            "CREATE FUNCTION regrole_missing_language(value regrole DEFAULT '09') RETURNS integer LANGUAGE no_such_language AS 'SELECT 1'",
            "42704",
            "language \"no_such_language\" does not exist",
        ),
        (
            "CREATE FUNCTION regrole_bad_result(value regrole DEFAULT '23') RETURNS anyelement LANGUAGE SQL AS 'SELECT 1'",
            "42P13",
            "cannot determine result data type",
        ),
        (
            "CREATE FUNCTION regrole_bad_result_input(value regrole DEFAULT 'missing_regrole') RETURNS anyelement LANGUAGE SQL AS 'SELECT 1'",
            "42704",
            "role \"missing_regrole\" does not exist",
        ),
        (
            "CREATE FUNCTION regrole_bad_standard_body(value regrole DEFAULT '23') RETURNS integer LANGUAGE SQL RETURN missing_column + ('23'::regrole)::oid",
            "42703",
            "missing_column",
        ),
        (
            "CREATE FUNCTION regrole_bad_body_role(value regrole DEFAULT '23') RETURNS regrole LANGUAGE SQL RETURN 'missing_regrole'::regrole",
            "42704",
            "role \"missing_regrole\" does not exist",
        ),
        (
            "CREATE FUNCTION regrole_bad_body_oid(value regrole DEFAULT '23') RETURNS regrole LANGUAGE SQL RETURN '09'::regrole",
            "22P02",
            "invalid input syntax for type oid",
        ),
        (
            "CREATE FUNCTION regrole_plpgsql_standard(value regrole DEFAULT '23') RETURNS integer LANGUAGE plpgsql RETURN 1",
            "42P13",
            "inline SQL function body only valid for language SQL",
        ),
        (
            "CREATE FUNCTION regrole_bad_source(value regrole DEFAULT '23') RETURNS integer LANGUAGE SQL AS 'SELECT +'",
            "0A000",
            "constant of the type regrole cannot be used here",
        ),
    ] {
        let error = eng.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some(state), "{sql}: {error}");
        assert!(error.to_string().contains(detail), "{sql}: {error}");
    }
}

#[test]
fn pg18_regrole_constant_exceptions_remain_storable() {
    let eng = engine();
    eng.sql(
        "CREATE FUNCTION valid_regrole_source_body() RETURNS regrole LANGUAGE SQL IMMUTABLE AS 'SELECT ''23''::regrole'",
        &[],
    )
    .unwrap();
    assert_eq!(
        scalar(&eng, "SELECT valid_regrole_source_body()::oid"),
        Value::Int(23)
    );
    eng.sql(
        "CREATE TABLE valid_regrole_defaults (owner regrole DEFAULT ('uqa'::text)::regrole, owners regrole[] DEFAULT '{uqa}'::regrole[])",
        &[],
    )
    .unwrap();
    eng.sql(
        "INSERT INTO valid_regrole_defaults VALUES (DEFAULT, DEFAULT)",
        &[],
    )
    .unwrap();
    assert_eq!(
        text(
            &eng,
            "SELECT owner::text || ':' || owners::text FROM valid_regrole_defaults"
        ),
        "uqa:{uqa}"
    );
    eng.sql(
        "CREATE VIEW valid_regrole_view AS SELECT ('uqa'::text)::regrole AS runtime_owner, 10::regrole AS numeric_owner, NULL::regrole AS nullable_owner, '{uqa}'::regrole[] AS owners",
        &[],
    )
    .unwrap();
    assert_eq!(
        scalar(
            &eng,
            "SELECT runtime_owner::text = 'uqa' AND numeric_owner::oid = 10 AND nullable_owner IS NULL AND owners[1]::text = 'uqa' FROM valid_regrole_view"
        ),
        Value::Bool(true)
    );
}

#[test]
fn pg18_regrole_values_follow_transaction_and_reopen_catalog_state() {
    let directory = tempfile::TempDir::new().unwrap();
    let path = directory.path().join("regrole.db");
    let role_oid;
    {
        let eng = Engine::open(&path).unwrap();
        eng.sql("BEGIN", &[]).unwrap();
        eng.sql("CREATE ROLE rolled_back_regrole", &[]).unwrap();
        assert_eq!(
            text(&eng, "SELECT to_regrole('rolled_back_regrole')::text"),
            "rolled_back_regrole"
        );
        eng.sql("ROLLBACK", &[]).unwrap();
        assert_eq!(
            scalar(&eng, "SELECT to_regrole('rolled_back_regrole') IS NULL"),
            Value::Bool(true)
        );
        eng.sql("CREATE ROLE durable_regrole", &[]).unwrap();
        role_oid = scalar(&eng, "SELECT to_regrole('durable_regrole')::oid");
        eng.sql(
            "CREATE TABLE regrole_values (owner regrole, owners regrole[])",
            &[],
        )
        .unwrap();
        let error = eng
            .sql(
                "INSERT INTO regrole_values VALUES ('missing_regrole', '{missing_regrole}')",
                &[],
            )
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("42704"));
        eng.sql(
            "INSERT INTO regrole_values VALUES ('durable_regrole', '{durable_regrole}')",
            &[],
        )
        .unwrap();
    }
    let reopened = Engine::open(&path).unwrap();
    assert_eq!(
        scalar(&reopened, "SELECT to_regrole('durable_regrole')::oid"),
        role_oid
    );
    assert_eq!(
        text(&reopened, "SELECT owner::text FROM regrole_values"),
        "durable_regrole"
    );
    assert_eq!(
        text(&reopened, "SELECT owners::text FROM regrole_values"),
        "{durable_regrole}"
    );
    reopened.sql("DROP ROLE durable_regrole", &[]).unwrap();
    assert_eq!(
        scalar(&reopened, "SELECT to_regrole('durable_regrole') IS NULL"),
        Value::Bool(true)
    );
    let Value::Int(role_oid) = role_oid else {
        unreachable!()
    };
    assert_eq!(
        text(&reopened, "SELECT owner::text FROM regrole_values"),
        role_oid.to_string()
    );
    assert_eq!(
        text(&reopened, "SELECT owners::text FROM regrole_values"),
        format!("{{{role_oid}}}")
    );
}

fn assert_soft_lookup_results(eng: &Engine) {
    for sql in [
        "SELECT to_regproc('missing') IS NULL",
        "SELECT to_regprocedure('missing(integer)') IS NULL",
        "SELECT to_regprocedure('casefold') IS NULL",
        "SELECT to_regclass('missing') IS NULL",
        "SELECT to_regnamespace('missing') IS NULL",
        "SELECT to_regrole('missing') IS NULL",
        "SELECT to_regtype('missing') IS NULL",
        "SELECT to_regclass('foo,bar') IS NULL",
        "SELECT to_regproc('foo,bar') IS NULL",
        "SELECT to_regprocedure('foo,bar()') IS NULL",
        "SELECT to_regnamespace('foo,bar') IS NULL",
        "SELECT to_regclass('\"unterminated') IS NULL",
        "SELECT to_regproc('\"unterminated') IS NULL",
        "SELECT to_regprocedure('\"unterminated()') IS NULL",
        "SELECT to_regnamespace('\"unterminated') IS NULL",
        "SELECT to_regnamespace('one.two.three') IS NULL",
        "SELECT to_regnamespace('one.two.three.four') IS NULL",
        "SELECT to_regtype('SETOF integer') IS NULL",
    ] {
        assert_eq!(scalar(eng, sql), Value::Bool(true), "{sql}");
    }
    for function in [
        "to_regproc",
        "to_regprocedure",
        "to_regclass",
        "to_regnamespace",
        "to_regrole",
        "to_regtype",
    ] {
        assert_eq!(
            scalar(eng, &format!("SELECT {function}(NULL)")),
            Value::Null,
            "{function}"
        );
    }
    for function in [
        "to_regproc",
        "to_regprocedure",
        "to_regclass",
        "to_regnamespace",
        "to_regrole",
        "to_regtype",
    ] {
        for (input, oid) in [("23", 23), ("00023", 19), ("-", 0)] {
            assert_eq!(
                scalar(eng, &format!("SELECT {function}('{input}')::oid")),
                Value::Int(oid),
                "{function}({input})"
            );
        }
        assert_eq!(
            scalar(eng, &format!("SELECT {function}('09') IS NULL")),
            Value::Bool(true),
            "{function}(09)"
        );
    }
}

fn assert_lookup_errors(eng: &Engine) {
    for (sql, state, detail) in [
        (
            "SELECT to_regclass('one.two.three')",
            "0A000",
            Some("cross-database references are not implemented: \"one.two.three\""),
        ),
        (
            "SELECT to_regproc('one.two.three')",
            "0A000",
            Some("cross-database references are not implemented: one.two.three"),
        ),
        (
            "SELECT to_regprocedure('one.two.three()')",
            "0A000",
            Some("cross-database references are not implemented: one.two.three"),
        ),
        (
            "SELECT to_regtype('one.two.three')",
            "0A000",
            Some("cross-database references are not implemented: one.two.three"),
        ),
        (
            "SELECT to_regclass('one.two.three.four')",
            "42601",
            Some("improper relation name (too many dotted names): one.two.three.four"),
        ),
        (
            "SELECT to_regproc('one.two.three.four')",
            "42601",
            Some("improper qualified name (too many dotted names): one.two.three.four"),
        ),
        (
            "SELECT to_regprocedure('one.two.three.four()')",
            "42601",
            Some("improper qualified name (too many dotted names): one.two.three.four"),
        ),
        (
            "SELECT to_regtype('one.two.three.four')",
            "42601",
            Some("improper qualified name (too many dotted names): one.two.three.four"),
        ),
        ("SELECT to_regtype('integer[')", "42601", None),
        ("SELECT to_regtype('foo,bar')", "42601", None),
        ("SELECT to_regclass(1)", "42883", None),
        ("SELECT to_regproc()", "42883", None),
        (
            "SELECT to_regprocedure('casefold(text)', 'extra')",
            "42883",
            None,
        ),
        ("SELECT to_regnamespace(value => 'public')", "42883", None),
    ] {
        let error = eng.sql(sql, &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some(state), "{sql}: {error}");
        if let Some(detail) = detail {
            assert!(error.to_string().contains(detail), "{sql}: {error}");
        }
    }
    let error = eng
        .sql(
            "CREATE TABLE invalid_reg_lookup_generation (name TEXT, object regclass GENERATED ALWAYS AS (to_regclass(name)) STORED)",
            &[],
        )
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42P17"));
    assert!(error.to_string().contains("not immutable"), "{error}");
}

#[test]
fn pg18_to_reg_lookups_preserve_soft_failures_and_sqlstates() {
    let eng = engine();
    assert_soft_lookup_results(&eng);
    assert_lookup_errors(&eng);
}

#[test]
fn pg18_to_reg_lookups_follow_fixed_builtin_search_path_resolution() {
    let eng = engine();
    for sql in [
        "CREATE SCHEMA reg_lookup_shadow",
        "CREATE ROLE reg_lookup_shadow_target",
        "CREATE FUNCTION reg_lookup_shadow.to_regclass(value TEXT) RETURNS regclass LANGUAGE SQL IMMUTABLE AS 'SELECT 1259::oid::regclass'",
        "CREATE FUNCTION reg_lookup_shadow.to_regrole(value TEXT) RETURNS regrole LANGUAGE SQL IMMUTABLE AS 'SELECT 10::oid::regrole'",
        "SET search_path = reg_lookup_shadow, public",
    ] {
        eng.sql(sql, &[]).unwrap();
    }
    assert_eq!(
        scalar(&eng, "SELECT to_regclass('pg_type')::oid"),
        Value::Int(1247)
    );
    let target_oid = scalar(
        &eng,
        "SELECT pg_catalog.to_regrole('reg_lookup_shadow_target')::oid",
    );
    assert_eq!(
        scalar(&eng, "SELECT to_regrole('reg_lookup_shadow_target')::oid"),
        target_oid
    );
    eng.sql(
        "SET search_path = reg_lookup_shadow, pg_catalog, public",
        &[],
    )
    .unwrap();
    assert_eq!(
        scalar(&eng, "SELECT to_regclass('pg_type')::oid"),
        Value::Int(1259)
    );
    assert_eq!(
        scalar(&eng, "SELECT to_regrole('reg_lookup_shadow_target')::oid"),
        Value::Int(10)
    );
    assert_eq!(
        scalar(&eng, "SELECT pg_catalog.to_regclass('pg_type')::oid"),
        Value::Int(1247)
    );
    assert_eq!(
        scalar(
            &eng,
            "SELECT pg_catalog.to_regrole('reg_lookup_shadow_target')::oid"
        ),
        target_oid
    );
}

#[test]
fn pg18_regrole_type_and_io_catalog_rows_match_postgresql_18() {
    let eng = engine();
    let types = eng
        .sql(
            "SELECT oid, typname, typlen, typbyval, typtype, typcategory, typispreferred, typdelim, typrelid, typelem, typarray, typinput::oid AS typinput, typoutput::oid AS typoutput, typreceive::oid AS typreceive, typsend::oid AS typsend, typalign, typstorage, typcollation FROM pg_catalog.pg_type WHERE oid IN (4096,4097) ORDER BY oid",
            &[],
        )
        .unwrap()
        .rows;
    assert_eq!(types.len(), 2);
    for (column, value) in [
        ("oid", Value::Int(4096)),
        ("typname", Value::Str("regrole".into())),
        ("typlen", Value::Int(4)),
        ("typbyval", Value::Bool(true)),
        ("typtype", Value::Str("b".into())),
        ("typcategory", Value::Str("N".into())),
        ("typispreferred", Value::Bool(false)),
        ("typdelim", Value::Str(",".into())),
        ("typrelid", Value::Int(0)),
        ("typelem", Value::Int(0)),
        ("typarray", Value::Int(4097)),
        ("typinput", Value::Int(4098)),
        ("typoutput", Value::Int(4092)),
        ("typreceive", Value::Int(4094)),
        ("typsend", Value::Int(4095)),
        ("typalign", Value::Str("i".into())),
        ("typstorage", Value::Str("p".into())),
        ("typcollation", Value::Int(0)),
    ] {
        assert_eq!(types[0][column], value, "pg_type.regrole.{column}");
    }
    for (column, value) in [
        ("oid", Value::Int(4097)),
        ("typname", Value::Str("_regrole".into())),
        ("typlen", Value::Int(-1)),
        ("typbyval", Value::Bool(false)),
        ("typcategory", Value::Str("A".into())),
        ("typelem", Value::Int(4096)),
        ("typarray", Value::Int(0)),
        ("typinput", Value::Int(750)),
        ("typoutput", Value::Int(751)),
        ("typreceive", Value::Int(2400)),
        ("typsend", Value::Int(2401)),
        ("typalign", Value::Str("i".into())),
        ("typstorage", Value::Str("x".into())),
    ] {
        assert_eq!(types[1][column], value, "pg_type._regrole.{column}");
    }

    let routines = eng
        .sql(
            "SELECT oid, proname, prorettype, proargtypes::text AS proargtypes, proisstrict, provolatile, proparallel, proleakproof, prosrc, proargnames FROM pg_catalog.pg_proc WHERE oid IN (4092,4094,4095,4098) ORDER BY oid",
            &[],
        )
        .unwrap()
        .rows;
    for (row, (oid, name, return_type, arguments, volatility)) in routines.iter().zip([
        (4092, "regroleout", 2275, "4096", "s"),
        (4094, "regrolerecv", 4096, "2281", "i"),
        (4095, "regrolesend", 17, "4096", "i"),
        (4098, "regrolein", 4096, "2275", "s"),
    ]) {
        assert_eq!(row["oid"], Value::Int(oid));
        assert_eq!(row["proname"], Value::Str(name.into()));
        assert_eq!(row["prorettype"], Value::Int(return_type));
        assert_eq!(row["proargtypes"], Value::Str(arguments.into()));
        assert_eq!(row["proisstrict"], Value::Bool(true));
        assert_eq!(row["provolatile"], Value::Str(volatility.into()));
        assert_eq!(row["proparallel"], Value::Str("s".into()));
        assert_eq!(row["proleakproof"], Value::Bool(false));
        assert_eq!(row["prosrc"], Value::Str(name.into()));
        assert_eq!(row["proargnames"], Value::Null);
    }
}

#[test]
fn pg18_to_reg_lookups_catalog_rows_match_postgresql_18() {
    let eng = engine();
    let rows = eng
        .sql(
            "SELECT oid, proname, prorettype, proargtypes, proisstrict, provolatile, proparallel, proleakproof, prosrc, proargnames FROM pg_catalog.pg_proc WHERE proname IN ('to_regclass','to_regproc','to_regprocedure','to_regnamespace','to_regrole','to_regtype') ORDER BY oid",
            &[],
        )
        .unwrap()
        .rows;
    let expected = [
        (3479, "to_regprocedure", 2202),
        (3493, "to_regtype", 2206),
        (3494, "to_regproc", 24),
        (3495, "to_regclass", 2205),
        (4086, "to_regnamespace", 4089),
        (4093, "to_regrole", 4096),
    ];
    assert_eq!(rows.len(), expected.len());
    for (row, (oid, name, return_type)) in rows.iter().zip(expected) {
        assert_eq!(row["oid"], Value::Int(oid));
        assert_eq!(row["proname"], Value::Str(name.into()));
        assert_eq!(row["prorettype"], Value::Int(return_type));
        assert_eq!(row["proargtypes"], Value::List(vec![Value::Int(25)]));
        assert_eq!(row["proisstrict"], Value::Bool(true));
        assert_eq!(row["provolatile"], Value::Str("s".into()));
        assert_eq!(row["proparallel"], Value::Str("s".into()));
        assert_eq!(row["proleakproof"], Value::Bool(false));
        assert_eq!(row["prosrc"], Value::Str(name.into()));
        assert_eq!(row["proargnames"], Value::Null);
    }

    let routines = eng
        .sql(
            "SELECT routine_name, data_type, type_udt_schema, type_udt_name, is_deterministic, external_language FROM information_schema.routines WHERE routine_name IN ('to_regclass','to_regproc','to_regprocedure','to_regnamespace','to_regrole','to_regtype') ORDER BY routine_name",
            &[],
        )
        .unwrap()
        .rows;
    assert_eq!(routines.len(), expected.len());
    for row in routines {
        let Value::Str(name) = &row["routine_name"] else {
            panic!("routine_name must be text")
        };
        let alias = name.strip_prefix("to_").expect("to_reg* routine name");
        assert_eq!(row["data_type"], Value::Str(alias.into()));
        assert_eq!(row["type_udt_schema"], Value::Str("pg_catalog".into()));
        assert_eq!(row["type_udt_name"], Value::Str(alias.into()));
        assert_eq!(row["is_deterministic"], Value::Str("NO".into()));
        assert_eq!(row["external_language"], Value::Str("INTERNAL".into()));
    }
}
