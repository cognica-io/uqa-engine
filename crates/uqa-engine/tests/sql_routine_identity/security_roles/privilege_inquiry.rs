//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 function privilege inquiry, namespace binding and revocable owner execution.

use super::*;

fn setup() -> Engine {
    let engine = Engine::new();
    for sql in [
        "CREATE ROLE routine_owner",
        "CREATE ROLE routine_reader",
        "CREATE ROLE routine_member INHERIT",
        "GRANT routine_reader TO routine_member",
        "CREATE SCHEMA routine_schema AUTHORIZATION routine_owner",
        "SET ROLE routine_owner",
        "CREATE FUNCTION routine_schema.f(value integer) RETURNS integer RETURN value + 1",
        "CREATE PROCEDURE routine_schema.p() LANGUAGE SQL AS 'SELECT 1'",
        "RESET ROLE",
    ] {
        engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    engine
}

fn inquiry(engine: &Engine, arguments: &str) -> Value {
    scalar(
        engine,
        &format!("SELECT has_function_privilege({arguments}) AS v"),
    )
}

#[test]
fn pg18_function_privilege_inquiry_covers_all_overloads_and_namespace_authorization() {
    let engine = setup();
    let function_oid = scalar(
        &engine,
        "SELECT 'routine_schema.f(integer)'::regprocedure::oid AS v",
    );
    let role_oid = scalar(
        &engine,
        "SELECT oid AS v FROM pg_roles WHERE rolname = 'routine_reader'",
    );
    let (Value::Int(function_oid), Value::Int(role_oid)) = (function_oid, role_oid) else {
        panic!("expected OIDs");
    };
    engine.sql("SET ROLE routine_reader", &[]).unwrap();
    assert_eq!(
        inquiry(&engine, &format!("{function_oid}::oid, 'EXECUTE'")),
        Value::Bool(true)
    );
    for subject in ["", "'routine_reader',", "'routine_owner',"] {
        assert_eq!(sqlstate(&engine, &format!("SELECT has_function_privilege({subject}'routine_schema.f(integer)', 'EXECUTE')")), "42501");
    }
    engine.sql("RESET ROLE", &[]).unwrap();
    engine
        .sql(
            "GRANT USAGE ON SCHEMA routine_schema TO routine_reader",
            &[],
        )
        .unwrap();
    engine.sql("SET ROLE routine_reader", &[]).unwrap();
    for subject in [
        String::new(),
        "'routine_reader'::name,".into(),
        format!("{role_oid}::oid,"),
    ] {
        for target in [
            "'routine_schema.f(integer)'::text".into(),
            format!("{function_oid}::oid"),
        ] {
            assert_eq!(
                inquiry(&engine, &format!("{subject}{target}, 'EXECUTE'")),
                Value::Bool(true)
            );
            assert_eq!(
                inquiry(
                    &engine,
                    &format!("{subject}{target}, 'EXECUTE WITH GRANT OPTION'")
                ),
                Value::Bool(false)
            );
        }
    }
    assert_eq!(
        inquiry(&engine, "'routine_schema.p()', 'EXECUTE'"),
        Value::Bool(true)
    );
    engine.sql("PREPARE routine_inquiry(name, oid, text) AS SELECT has_function_privilege($1, $2, $3) AS v", &[]).unwrap();
    assert_eq!(
        scalar(
            &engine,
            &format!("EXECUTE routine_inquiry('routine_reader', {function_oid}, 'EXECUTE')")
        ),
        Value::Bool(true)
    );
    assert_eq!(scalar(&engine, "SELECT pg_typeof(has_function_privilege('routine_schema.f(integer)', 'EXECUTE'))::text AS v"), Value::Str("boolean".into()));
}

#[test]
fn pg18_function_privilege_inquiry_matches_missing_values_and_error_precedence() {
    let engine = setup();
    for (arguments, expected) in [
        (
            "4294967295::oid, 'pg_catalog.abs(integer)', 'EXECUTE'",
            Value::Bool(true),
        ),
        (
            "'public', 'routine_schema.f(integer)', 'EXECUTE'",
            Value::Bool(true),
        ),
        ("'routine_reader', 4294967295::oid, 'EXECUTE'", Value::Null),
        ("'uqa', 4294967295::oid, 'EXECUTE'", Value::Bool(true)),
        ("'uqa', '4294967295', 'EXECUTE'", Value::Bool(true)),
        ("'absent_role', NULL::text, 'INVALID'", Value::Null),
        (
            "'routine_reader', 'routine_schema.f(integer)', 'EXECUTE WITH GRANT OPTION, execute'",
            Value::Bool(true),
        ),
    ] {
        assert_eq!(inquiry(&engine, arguments), expected, "{arguments}");
    }
    for (arguments, expected) in [
        ("'PUBLIC', 'routine_schema.f(integer)', 'EXECUTE'", "42704"),
        (
            "'absent_role', 'routine_schema.f(integer)', 'EXECUTE'",
            "42704",
        ),
        ("'routine_reader', 'missing_fn()', 'EXECUTE'", "42883"),
        ("'routine_reader', 'abs', 'EXECUTE'", "22P02"),
        ("'routine_reader', 'abs(', 'EXECUTE'", "22P02"),
        ("'routine_reader', 'abs(no_such_type)', 'EXECUTE'", "42704"),
        ("'routine_reader', 'absent_schema.f()', 'EXECUTE'", "42883"),
        ("'routine_reader', '4294967295', 'EXECUTE'", "XX000"),
        (
            "'routine_reader', 'routine_schema.f(integer)', 'SELECT'",
            "22023",
        ),
        ("'routine_reader', 'routine_schema.f(integer)', ''", "22023"),
        (
            "'routine_reader', 'routine_schema.f(integer)', 'EXECUTE  WITH GRANT OPTION'",
            "22023",
        ),
        (
            "'routine_reader', 'routine_schema.f(integer)', 'EXECUTE,'",
            "22023",
        ),
        ("'routine_reader', 'missing_fn()', 'INVALID'", "42883"),
        ("'routine_reader', 4294967295::oid, 'INVALID'", "22023"),
        ("'uqa', '0', 'EXECUTE'", "42883"),
        ("'uqa', '-', 'EXECUTE'", "42883"),
    ] {
        assert_eq!(
            sqlstate(
                &engine,
                &format!("SELECT has_function_privilege({arguments})")
            ),
            expected,
            "{arguments}"
        );
    }
}

#[test]
fn pg18_owner_execute_revocation_changes_calls_inquiry_and_catalog_without_losing_grant_options() {
    let engine = setup();
    for sql in [
        "REVOKE EXECUTE ON FUNCTION routine_schema.f(integer) FROM PUBLIC",
        "GRANT EXECUTE ON FUNCTION routine_schema.f(integer) TO routine_reader WITH GRANT OPTION",
        "SET ROLE routine_owner",
        "REVOKE EXECUTE ON FUNCTION routine_schema.f(integer) FROM routine_owner",
    ] {
        engine.sql(sql, &[]).unwrap();
    }
    assert_eq!(
        inquiry(&engine, "'routine_schema.f(integer)', 'EXECUTE'"),
        Value::Bool(false)
    );
    assert_eq!(
        inquiry(
            &engine,
            "'routine_schema.f(integer)', 'EXECUTE WITH GRANT OPTION'"
        ),
        Value::Bool(true)
    );
    assert_eq!(sqlstate(&engine, "SELECT routine_schema.f(1)"), "42501");
    assert_eq!(
        inquiry(
            &engine,
            "'routine_member', 'routine_schema.f(integer)', 'EXECUTE WITH GRANT OPTION'"
        ),
        Value::Bool(true)
    );
    assert_eq!(
        inquiry(
            &engine,
            "4294967295::oid, 'routine_schema.f(integer)', 'EXECUTE'"
        ),
        Value::Bool(false)
    );
    let acl = scalar(
        &engine,
        "SELECT proacl AS v FROM pg_proc WHERE proname = 'f'",
    );
    assert_eq!(
        acl,
        Value::Array(
            uqa_core::ArrayValue::try_new(vec![Value::Str(
                "routine_reader=X*/routine_owner".into()
            )])
            .unwrap()
        )
    );
    engine
        .sql(
            "GRANT EXECUTE ON FUNCTION routine_schema.f(integer) TO routine_owner",
            &[],
        )
        .unwrap();
    assert_eq!(
        scalar(&engine, "SELECT routine_schema.f(1) AS v"),
        Value::Int(2)
    );
    engine.sql("REVOKE GRANT OPTION FOR EXECUTE ON FUNCTION routine_schema.f(integer) FROM routine_owner", &[]).unwrap();
    assert_eq!(
        inquiry(
            &engine,
            "'routine_schema.f(integer)', 'EXECUTE WITH GRANT OPTION'"
        ),
        Value::Bool(true)
    );
    engine
        .sql(
            "REVOKE EXECUTE ON PROCEDURE routine_schema.p() FROM PUBLIC, routine_owner",
            &[],
        )
        .unwrap();
    assert_eq!(sqlstate(&engine, "CALL routine_schema.p()"), "42501");
    assert_eq!(
        inquiry(&engine, "'routine_schema.p()', 'EXECUTE'"),
        Value::Bool(false)
    );
}

#[test]
fn pg18_function_privilege_catalog_identifies_all_six_overloads() {
    let engine = Engine::new();
    let rows = engine.sql("SELECT oid, proargtypes, prosrc, prorettype, proisstrict, provolatile, proparallel FROM pg_proc WHERE proname = 'has_function_privilege' ORDER BY oid", &[]).unwrap().rows;
    assert_eq!(rows.len(), 6);
    for (row, (oid, types, suffix)) in rows.iter().zip([
        (2256, vec![19, 25, 25], "name_name"),
        (2257, vec![19, 26, 25], "name_id"),
        (2258, vec![26, 25, 25], "id_name"),
        (2259, vec![26, 26, 25], "id_id"),
        (2260, vec![25, 25], "name"),
        (2261, vec![26, 25], "id"),
    ]) {
        assert_eq!(row["oid"], Value::Int(oid));
        assert_eq!(
            row["proargtypes"],
            crate::legacy_vectors::oidvector(types.into_iter().map(Value::Int).collect())
        );
        assert_eq!(
            row["prosrc"],
            Value::Str(format!("has_function_privilege_{suffix}"))
        );
        assert_eq!(row["prorettype"], Value::Int(16));
        assert_eq!(row["proisstrict"], Value::Bool(true));
        assert_eq!(row["provolatile"], Value::Str("s".into()));
        assert_eq!(row["proparallel"], Value::Str("s".into()));
    }
}

#[test]
fn pg18_function_privilege_signature_input_preserves_parser_diagnostics() {
    let engine = Engine::new();
    assert_eq!(
        inquiry(&engine, "'uqa.pg_catalog.abs(integer)', 'EXECUTE'"),
        Value::Bool(true)
    );
    for (signature, state) in [
        ("elsewhere.pg_catalog.abs(integer)", "0A000"),
        (".abs(integer)", "42602"),
        ("abs(integer,)", "22P02"),
        ("abs(999)", "42601"),
        ("08", "22P02"),
        ("4294967296", "22003"),
        ("", "22P02"),
    ] {
        assert_eq!(
            sqlstate(
                &engine,
                &format!("SELECT has_function_privilege('{signature}', 'EXECUTE')")
            ),
            state,
            "{signature}"
        );
    }
    let signature = format!("abs({})", vec!["integer"; 101].join(","));
    assert_eq!(
        sqlstate(
            &engine,
            &format!("SELECT has_function_privilege('{signature}', 'EXECUTE')")
        ),
        "54023"
    );
    assert_eq!(
        scalar(
            &engine,
            &format!("SELECT to_regprocedure('{signature}') AS v")
        ),
        Value::Null
    );
}

#[test]
fn pg18_function_privilege_builtin_signatures_share_exact_catalog_oids() {
    let engine = Engine::new();
    engine.sql("CREATE ROLE builtin_reader", &[]).unwrap();
    for (oid, type_name) in [
        (1394, "real"),
        (1395, "double precision"),
        (1396, "bigint"),
        (1397, "integer"),
        (1398, "smallint"),
        (1705, "numeric"),
    ] {
        let signature = format!("pg_catalog.abs({type_name})");
        assert_eq!(
            scalar(
                &engine,
                &format!("SELECT '{signature}'::regprocedure::oid AS v")
            ),
            Value::Int(oid)
        );
        for target in [format!("'{signature}'"), format!("{oid}::oid")] {
            assert_eq!(
                inquiry(&engine, &format!("'builtin_reader', {target}, 'EXECUTE'")),
                Value::Bool(true)
            );
            assert_eq!(
                inquiry(
                    &engine,
                    &format!("'builtin_reader', {target}, 'EXECUTE WITH GRANT OPTION'")
                ),
                Value::Bool(false)
            );
        }
    }
    assert_eq!(
        scalar(
            &engine,
            "SELECT count(*) AS v FROM pg_proc WHERE proname = 'abs'"
        ),
        Value::Int(6)
    );
}
