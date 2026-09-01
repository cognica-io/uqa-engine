//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn postgresql_18_regtype_aliases_use_catalog_aware_text_output() {
    let engine = Engine::new();
    let result = engine
        .sql(
            "SELECT (0::regproc)::text AS zero_proc, (0::regclass)::text AS zero_class, \
                    (0::regnamespace)::text AS zero_namespace, (0::regtype)::text AS zero_type, \
                    (1574::regproc)::text AS sequence_proc_name, (2559::regproc)::text AS lastval_proc_name, (1598::regproc)::text AS proc_name, \
                    (1259::regclass)::text AS class_name, \
                    (11::regnamespace)::text AS namespace_name, (23::regtype)::text AS type_name, \
                    (999999::regclass)::text AS missing_name, 'pg_class'::regclass AS class_oid, \
                    NULL::regproc::text AS null_proc, \
                    ARRAY[0::regproc, 1598::regproc, 999999::regproc]::text AS proc_array",
            &[],
        )
        .unwrap();
    let row = &result.rows[0];
    for column in ["zero_proc", "zero_class", "zero_namespace", "zero_type"] {
        assert_eq!(row[column], Value::Str("-".into()), "{column}");
    }
    assert_eq!(row["sequence_proc_name"], Value::Str("nextval".into()));
    assert_eq!(row["lastval_proc_name"], Value::Str("lastval".into()));
    assert_eq!(row["proc_name"], Value::Str("pg_catalog.random".into()));
    assert_eq!(row["class_name"], Value::Str("pg_class".into()));
    assert_eq!(row["namespace_name"], Value::Str("pg_catalog".into()));
    assert_eq!(row["type_name"], Value::Str("integer".into()));
    assert_eq!(row["missing_name"], Value::Str("999999".into()));
    assert_eq!(row["class_oid"], Value::Int(1259));
    assert_eq!(row["null_proc"], Value::Null);
    assert_eq!(
        row["proc_array"],
        Value::Str("{-,pg_catalog.random,999999}".into())
    );

    let mut copy = Vec::new();
    engine
        .copy_to(
            "COPY (SELECT 0::regproc, 1598::regproc, 1259::regclass, 11::regnamespace, 23::regtype, ARRAY[0::regclass, 1259::regclass, 999999::regclass]) TO STDOUT",
            &mut copy,
        )
        .unwrap();
    assert_eq!(
        String::from_utf8(copy).unwrap(),
        "-\tpg_catalog.random\tpg_class\tpg_catalog\tinteger\t{-,pg_class,999999}\n"
    );

    engine
        .sql("CREATE TABLE public.pg_class (id INTEGER)", &[])
        .unwrap();
    let implicit_catalog = engine
        .sql("SELECT 'pg_class'::regclass AS oid", &[])
        .unwrap();
    assert_eq!(implicit_catalog.rows[0]["oid"], Value::Int(1259));
    engine
        .sql("SET search_path TO public, pg_catalog", &[])
        .unwrap();
    let shadowed = engine
        .sql(
            "SELECT 'pg_class'::regclass AS oid, ('pg_class'::regclass)::text AS visible_name, (1259::regclass)::text AS catalog_name",
            &[],
        )
        .unwrap();
    assert_ne!(shadowed.rows[0]["oid"], Value::Int(1259));
    assert_eq!(
        shadowed.rows[0]["visible_name"],
        Value::Str("pg_class".into())
    );
    assert_eq!(
        shadowed.rows[0]["catalog_name"],
        Value::Str("pg_catalog.pg_class".into())
    );
}

#[test]
fn postgresql_18_sequence_routine_identities_match_catalog() {
    let engine = Engine::new();
    let sequence_routines = engine
        .sql(
            "SELECT oid, proname, prorettype, proargtypes, proisstrict, provolatile, proparallel, prosrc FROM pg_proc WHERE oid IN (1574, 1575, 1576, 1765, 2559) ORDER BY oid",
            &[],
        )
        .unwrap();
    assert_eq!(sequence_routines.rows.len(), 5);
    assert_eq!(
        sequence_routines.rows[0]["proname"],
        Value::Str("nextval".into())
    );
    assert_eq!(
        sequence_routines.rows[1]["proname"],
        Value::Str("currval".into())
    );
    for row in &sequence_routines.rows {
        assert_eq!(row["prorettype"], Value::Int(20));
        assert_eq!(row["proisstrict"], Value::Bool(true));
        assert_eq!(row["provolatile"], Value::Str("v".into()));
        assert_eq!(row["proparallel"], Value::Str("u".into()));
    }
    assert_eq!(
        sequence_routines.rows[0]["proargtypes"],
        Value::List(vec![Value::Int(2205)])
    );
    assert_eq!(
        sequence_routines.rows[2]["proargtypes"],
        Value::List(vec![Value::Int(2205), Value::Int(20)])
    );
    assert_eq!(
        sequence_routines.rows[3]["proargtypes"],
        Value::List(vec![Value::Int(2205), Value::Int(20), Value::Int(16)])
    );
    assert_eq!(
        sequence_routines.rows[4]["proname"],
        Value::Str("lastval".into())
    );
    assert_eq!(
        sequence_routines.rows[4]["proargtypes"],
        Value::List(Vec::new())
    );
    assert_eq!(
        sequence_routines.rows[4]["prosrc"],
        Value::Str("lastval".into())
    );
}

#[test]
fn regtype_output_cache_invalidates_for_table_ddl_and_rollback() {
    let engine = Engine::new();
    engine
        .sql("SELECT (1259::regclass)::text AS name", &[])
        .unwrap();
    engine
        .sql(
            "CREATE TABLE public.regclass_cache_committed (id INTEGER)",
            &[],
        )
        .unwrap();
    let committed = engine
        .sql(
            "SELECT ('regclass_cache_committed'::regclass)::text AS name",
            &[],
        )
        .unwrap();
    assert_eq!(
        committed.rows[0]["name"],
        Value::Str("regclass_cache_committed".into())
    );
    engine.sql("BEGIN", &[]).unwrap();
    engine
        .sql(
            "CREATE TABLE public.regclass_cache_rollback (id INTEGER)",
            &[],
        )
        .unwrap();
    let cached = engine
        .sql(
            "SELECT 'regclass_cache_rollback'::regclass AS oid, ('regclass_cache_rollback'::regclass)::text AS name",
            &[],
        )
        .unwrap();
    assert_eq!(
        cached.rows[0]["name"],
        Value::Str("regclass_cache_rollback".into())
    );
    let Value::Int(rolled_back_oid) = cached.rows[0]["oid"] else {
        panic!("regclass OID must use its integer carrier");
    };
    engine.sql("ROLLBACK", &[]).unwrap();
    let after_rollback = engine
        .sql(
            &format!("SELECT ({rolled_back_oid}::regclass)::text AS name"),
            &[],
        )
        .unwrap();
    assert_eq!(
        after_rollback.rows[0]["name"],
        Value::Str(rolled_back_oid.to_string())
    );
}

#[test]
fn postgresql_18_regprocedure_resolves_exact_routine_identities_and_catalog_metadata() {
    let engine = Engine::new();
    for sql in [
        "CREATE SCHEMA regprocedure_app",
        "CREATE FUNCTION regprocedure_app.identity_probe(value integer) RETURNS integer LANGUAGE SQL AS 'SELECT value'",
        "CREATE FUNCTION regprocedure_app.identity_probe(value text) RETURNS text LANGUAGE SQL AS 'SELECT value'",
        "CREATE FUNCTION public.identity_probe(value integer) RETURNS integer LANGUAGE SQL AS 'SELECT value + 1'",
        "CREATE PROCEDURE regprocedure_app.procedure_probe(value integer) LANGUAGE SQL AS 'SELECT value'",
        "SET search_path TO regprocedure_app, public, pg_catalog",
    ] {
        engine
            .sql(sql, &[])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }

    let visible = engine
        .sql(
            "SELECT pg_typeof('identity_probe(integer)'::regprocedure)::text AS type_name, \
                    'identity_probe(integer)'::regprocedure AS identity, \
                    ('identity_probe(integer)'::regprocedure)::text AS rendered, \
                    ('identity_probe(integer)'::regprocedure)::oid = \
                        (SELECT oid FROM pg_catalog.pg_proc WHERE proname = 'identity_probe' AND pronamespace = \
                            (SELECT oid FROM pg_catalog.pg_namespace WHERE nspname = 'regprocedure_app') AND proargtypes::text = '23') AS oid_matches, \
                    ('regprocedure_app.procedure_probe(integer)'::regprocedure)::text AS procedure_rendered, \
                    ('pg_catalog.md5(text)'::regprocedure)::oid AS builtin_oid, \
                    ('pg_catalog.md5(text)'::regprocedure)::text AS builtin_rendered, \
                    (0::regprocedure)::text AS zero_rendered",
            &[],
        )
        .unwrap();
    assert_eq!(visible.column_types[1], Some(ColumnType::Regprocedure));
    assert_eq!(
        visible.rows[0]["type_name"],
        Value::Str("regprocedure".into())
    );
    assert!(matches!(visible.rows[0]["identity"], Value::Int(_)));
    assert_eq!(
        visible.rows[0]["rendered"],
        Value::Str("identity_probe(integer)".into())
    );
    assert_eq!(visible.rows[0]["oid_matches"], Value::Bool(true));
    assert_eq!(
        visible.rows[0]["procedure_rendered"],
        Value::Str("procedure_probe(integer)".into())
    );
    assert_eq!(visible.rows[0]["builtin_oid"], Value::Int(2311));
    assert_eq!(
        visible.rows[0]["builtin_rendered"],
        Value::Str("md5(text)".into())
    );
    assert_eq!(visible.rows[0]["zero_rendered"], Value::Str("-".into()));

    engine
        .sql(
            "SET search_path TO public, regprocedure_app, pg_catalog",
            &[],
        )
        .unwrap();
    let shadowed = engine
        .sql(
            "SELECT ('identity_probe(integer)'::regprocedure)::oid = \
                        (SELECT oid FROM pg_catalog.pg_proc WHERE proname = 'identity_probe' AND pronamespace = \
                            (SELECT oid FROM pg_catalog.pg_namespace WHERE nspname = 'public') AND proargtypes::text = '23') AS public_selected, \
                    ('regprocedure_app.identity_probe(integer)'::regprocedure)::text AS app_rendered",
            &[],
        )
        .unwrap();
    assert_eq!(shadowed.rows[0]["public_selected"], Value::Bool(true));
    assert_eq!(
        shadowed.rows[0]["app_rendered"],
        Value::Str("regprocedure_app.identity_probe(integer)".into())
    );

    let missing = engine
        .sql(
            "SELECT 'regprocedure_app.identity_probe(bigint)'::regprocedure",
            &[],
        )
        .expect_err("missing overload must fail");
    assert_eq!(missing.sqlstate(), Some("42883"));

    assert_regprocedure_catalog_metadata(&engine);
}

fn assert_regprocedure_catalog_metadata(engine: &Engine) {
    let types = engine
        .sql(
            "SELECT oid, typname, typlen, typbyval, typtype, typcategory, typispreferred, typdelim, \
                    typrelid, typelem, typarray, typinput::oid AS typinput, typoutput::oid AS typoutput, \
                    typreceive::oid AS typreceive, typsend::oid AS typsend, typalign, typstorage \
             FROM pg_catalog.pg_type WHERE oid IN (2202, 2207) ORDER BY oid",
            &[],
        )
        .unwrap();
    assert_eq!(types.rows.len(), 2);
    assert_eq!(types.rows[0]["typname"], Value::Str("regprocedure".into()));
    for (column, value) in [
        ("oid", Value::Int(2202)),
        ("typlen", Value::Int(4)),
        ("typbyval", Value::Bool(true)),
        ("typtype", Value::Str("b".into())),
        ("typcategory", Value::Str("N".into())),
        ("typispreferred", Value::Bool(false)),
        ("typdelim", Value::Str(",".into())),
        ("typrelid", Value::Int(0)),
        ("typelem", Value::Int(0)),
        ("typarray", Value::Int(2207)),
        ("typinput", Value::Int(2212)),
        ("typoutput", Value::Int(2213)),
        ("typreceive", Value::Int(2446)),
        ("typsend", Value::Int(2447)),
        ("typalign", Value::Str("i".into())),
        ("typstorage", Value::Str("p".into())),
    ] {
        assert_eq!(types.rows[0][column], value, "pg_type.{column}");
    }
    assert_eq!(types.rows[1]["oid"], Value::Int(2207));
    assert_eq!(types.rows[1]["typname"], Value::Str("_regprocedure".into()));
    assert_eq!(types.rows[1]["typelem"], Value::Int(2202));
    assert_eq!(types.rows[1]["typarray"], Value::Int(0));
    assert_eq!(types.rows[1]["typcategory"], Value::Str("A".into()));

    let io_routines = engine
        .sql(
            "SELECT oid, proname, proisstrict, provolatile, proparallel, prorettype, proargtypes::text AS proargtypes, prosrc \
             FROM pg_catalog.pg_proc WHERE oid IN (2212, 2213, 2446, 2447) ORDER BY oid",
            &[],
        )
        .unwrap();
    assert_eq!(io_routines.rows.len(), 4);
    for (index, (oid, name, volatility, return_type, arguments)) in [
        (2212, "regprocedurein", "s", 2202, "2275"),
        (2213, "regprocedureout", "s", 2275, "2202"),
        (2446, "regprocedurerecv", "i", 2202, "2281"),
        (2447, "regproceduresend", "i", 17, "2202"),
    ]
    .into_iter()
    .enumerate()
    {
        let row = &io_routines.rows[index];
        assert_eq!(row["oid"], Value::Int(oid));
        assert_eq!(row["proname"], Value::Str(name.into()));
        assert_eq!(row["proisstrict"], Value::Bool(true));
        assert_eq!(row["provolatile"], Value::Str(volatility.into()));
        assert_eq!(row["proparallel"], Value::Str("s".into()));
        assert_eq!(row["prorettype"], Value::Int(return_type));
        assert_eq!(row["proargtypes"], Value::Str(arguments.into()));
        assert_eq!(row["prosrc"], Value::Str(name.into()));
    }
}
