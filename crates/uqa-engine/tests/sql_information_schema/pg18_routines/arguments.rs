//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn user_routine_catalog_row<'a>(
    result: &'a SQLResult,
    name: &str,
) -> &'a std::collections::BTreeMap<String, Value> {
    result
        .rows
        .iter()
        .find(|row| row["proname"] == Value::Str(name.into()))
        .unwrap_or_else(|| panic!("missing pg_proc row {name}"))
}

fn assert_plain_and_array_user_routine_catalog(result: &SQLResult) {
    let plain = user_routine_catalog_row(result, "cat_plain");
    assert_eq!(plain["pronargs"], Value::Int(1));
    assert_eq!(plain["prorettype"], Value::Int(23));
    assert_eq!(plain["proargtypes"], Value::List(vec![Value::Int(23)]));
    assert_eq!(plain["proallargtypes"], Value::Null);
    assert_eq!(plain["proargmodes"], Value::Null);
    assert_eq!(plain["proargnames"], Value::Null);

    let arrays = user_routine_catalog_row(result, "cat_arrays");
    assert_eq!(arrays["prorettype"], Value::Int(1009));
    assert_eq!(arrays["proargtypes"], Value::List(vec![Value::Int(1007)]));
}

fn assert_output_user_routine_catalog(result: &SQLResult) {
    let out = user_routine_catalog_row(result, "cat_out");
    assert_eq!(out["prorettype"], Value::Int(2249));
    assert_eq!(out["proargtypes"], Value::List(vec![Value::Int(23)]));
    assert_eq!(
        out["proallargtypes"],
        array(vec![Value::Int(23), Value::Int(23), Value::Int(25)])
    );
    assert_eq!(
        out["proargmodes"],
        array(vec![
            Value::Str("i".into()),
            Value::Str("o".into()),
            Value::Str("o".into())
        ])
    );
    assert_eq!(
        out["proargnames"],
        array(vec![
            Value::Str("a".into()),
            Value::Str("x".into()),
            Value::Str("y".into())
        ])
    );
}

fn assert_table_user_routine_catalog(result: &SQLResult) {
    let table = user_routine_catalog_row(result, "cat_table");
    assert_eq!(table["prorettype"], Value::Int(2249));
    assert_eq!(table["proretset"], Value::Bool(true));
    assert_eq!(
        table["proargmodes"],
        array(vec![
            Value::Str("i".into()),
            Value::Str("t".into()),
            Value::Str("t".into())
        ])
    );

    let table_one = user_routine_catalog_row(result, "cat_table_one");
    assert_eq!(table_one["prorettype"], Value::Int(20));
    assert_eq!(table_one["proretset"], Value::Bool(true));
    assert_eq!(
        table_one["proallargtypes"],
        array(vec![Value::Int(23), Value::Int(20)])
    );
    assert_eq!(
        table_one["proargmodes"],
        array(vec![Value::Str("i".into()), Value::Str("t".into())])
    );
}

fn assert_procedure_user_routine_catalog(result: &SQLResult) {
    let procedure = user_routine_catalog_row(result, "cat_proc");
    assert_eq!(procedure["prokind"], Value::Str("p".into()));
    assert_eq!(procedure["provariadic"], Value::Int(0));
    assert_eq!(procedure["pronargs"], Value::Int(1));
    assert_eq!(procedure["pronargdefaults"], Value::Int(1));
    assert_eq!(procedure["prorettype"], Value::Int(2249));
    assert_eq!(procedure["proargtypes"], Value::List(vec![Value::Int(23)]));
    assert_eq!(
        procedure["proallargtypes"],
        array(vec![Value::Int(25), Value::Int(23)])
    );
    assert_eq!(
        procedure["proargmodes"],
        array(vec![Value::Str("o".into()), Value::Str("i".into())])
    );
    assert_eq!(
        procedure["proargnames"],
        array(vec![Value::Str("y".into()), Value::Str("a".into())])
    );
    assert_eq!(procedure["proargdefaults"], Value::Str("7".into()));
}

fn assert_variadic_user_routine_catalog(result: &SQLResult) {
    let variadic = user_routine_catalog_row(result, "cat_variadic");
    assert_eq!(variadic["provariadic"], Value::Int(23));
    assert_eq!(variadic["pronargs"], Value::Int(2));
    assert_eq!(variadic["pronargdefaults"], Value::Int(1));
    assert_eq!(variadic["prorettype"], Value::Int(20));
    assert_eq!(
        variadic["proargtypes"],
        Value::List(vec![Value::Int(23), Value::Int(1007)])
    );
    assert_eq!(
        variadic["proallargtypes"],
        array(vec![Value::Int(23), Value::Int(1007), Value::Int(20)])
    );
    assert_eq!(
        variadic["proargmodes"],
        array(vec![
            Value::Str("i".into()),
            Value::Str("v".into()),
            Value::Str("o".into())
        ])
    );
    assert_eq!(
        variadic["proargnames"],
        array(vec![
            Value::Str("prefix".into()),
            Value::Str("items".into()),
            Value::Str("total".into())
        ])
    );
    assert_eq!(variadic["proargdefaults"], Value::Str("ARRAY[1, 2]".into()));
}

fn assert_polymorphic_variadic_user_routine_catalog(result: &SQLResult) {
    for (name, array_oid, element_oid) in [
        ("cat_poly_simple", 2277, 2283),
        ("cat_poly_common", 5078, 5077),
    ] {
        let polymorphic = user_routine_catalog_row(result, name);
        assert_eq!(polymorphic["provariadic"], Value::Int(element_oid));
        assert_eq!(polymorphic["pronargs"], Value::Int(1));
        assert_eq!(polymorphic["pronargdefaults"], Value::Int(0));
        assert_eq!(polymorphic["prorettype"], Value::Int(element_oid));
        assert_eq!(
            polymorphic["proargtypes"],
            Value::List(vec![Value::Int(array_oid)])
        );
        assert_eq!(
            polymorphic["proallargtypes"],
            array(vec![Value::Int(array_oid)])
        );
        assert_eq!(
            polymorphic["proargmodes"],
            array(vec![Value::Str("v".into())])
        );
        assert_eq!(polymorphic["proargdefaults"], Value::Null);
    }
}

fn assert_legacy_vector_variadic_user_routine_catalog(result: &SQLResult) {
    for (name, vector_oid, element_oid) in [("cat_int2vector", 22, 21), ("cat_oidvector", 30, 26)] {
        let variadic = user_routine_catalog_row(result, name);
        assert_eq!(variadic["provariadic"], Value::Int(element_oid));
        assert_eq!(variadic["pronargs"], Value::Int(1));
        assert_eq!(variadic["pronargdefaults"], Value::Int(0));
        assert_eq!(variadic["prorettype"], Value::Int(vector_oid));
        assert_eq!(
            variadic["proargtypes"],
            Value::List(vec![Value::Int(vector_oid)])
        );
        assert_eq!(
            variadic["proallargtypes"],
            array(vec![Value::Int(vector_oid)])
        );
        assert_eq!(variadic["proargmodes"], array(vec![Value::Str("v".into())]));
        assert_eq!(variadic["proargdefaults"], Value::Null);
    }
}

#[test]
fn postgresql_18_user_routine_catalog_preserves_argument_modes_and_type_oids() {
    let engine = Engine::new();
    for ddl in [
        "CREATE FUNCTION cat_plain(integer) RETURNS integer AS $$ BEGIN RETURN 1; END; $$ LANGUAGE plpgsql",
        "CREATE FUNCTION cat_out(a integer, OUT x integer, OUT y text) AS $$ BEGIN x := a; y := 'x'; END; $$ LANGUAGE plpgsql",
        "CREATE FUNCTION cat_table(a integer) RETURNS TABLE(x integer, y text) AS $$ BEGIN RETURN QUERY SELECT a, 'x'; END; $$ LANGUAGE plpgsql",
        "CREATE FUNCTION cat_table_one(value integer) RETURNS TABLE(item bigint) LANGUAGE SQL AS 'SELECT value::bigint'",
        "CREATE FUNCTION cat_arrays(integer[]) RETURNS text[] AS $$ BEGIN RETURN ARRAY['x']; END; $$ LANGUAGE plpgsql",
        "CREATE FUNCTION cat_variadic(prefix integer, VARIADIC items integer[] DEFAULT ARRAY[1,2], OUT total bigint) LANGUAGE SQL AS 'SELECT 1::bigint'",
        "CREATE FUNCTION cat_poly_simple(VARIADIC items anyarray) RETURNS anyelement LANGUAGE SQL AS 'SELECT NULL'",
        "CREATE FUNCTION cat_poly_common(VARIADIC items anycompatiblearray) RETURNS anycompatible LANGUAGE SQL AS 'SELECT NULL'",
        "CREATE FUNCTION cat_int2vector(VARIADIC items int2vector) RETURNS int2vector LANGUAGE SQL AS 'SELECT $1'",
        "CREATE FUNCTION cat_oidvector(VARIADIC items oidvector) RETURNS oidvector LANGUAGE SQL AS 'SELECT $1'",
        "CREATE PROCEDURE cat_proc(OUT y text, IN a integer DEFAULT 7) AS $$ BEGIN y := 'x'; END; $$ LANGUAGE plpgsql",
    ] {
        engine
            .sql(ddl, &[])
            .unwrap_or_else(|error| panic!("{ddl}: {error}"));
    }

    let result = engine
        .sql(
            "SELECT proname, prokind, provariadic, pronargs, pronargdefaults, prorettype, \
                    proretset, proargtypes, proallargtypes, proargmodes, proargnames, proargdefaults \
             FROM pg_catalog.pg_proc \
             WHERE proname IN ('cat_plain', 'cat_out', 'cat_table', 'cat_table_one', 'cat_arrays', \
                               'cat_variadic', 'cat_poly_simple', 'cat_poly_common', 'cat_int2vector', \
                               'cat_oidvector', 'cat_proc')",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows.len(), 11);
    assert_plain_and_array_user_routine_catalog(&result);
    assert_output_user_routine_catalog(&result);
    assert_table_user_routine_catalog(&result);
    assert_procedure_user_routine_catalog(&result);
    assert_variadic_user_routine_catalog(&result);
    assert_polymorphic_variadic_user_routine_catalog(&result);
    assert_legacy_vector_variadic_user_routine_catalog(&result);
}
