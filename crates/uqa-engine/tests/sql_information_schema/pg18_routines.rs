//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 built-in routine and database catalog regressions.

use uqa_core::{ArrayValue, Value};
use uqa_engine::{Engine, SQLResult};

fn array(values: Vec<Value>) -> Value {
    Value::Array(ArrayValue::try_new(values).expect("rectangular catalog array"))
}

pub(super) fn expected_information_schema_domain_layouts() -> Vec<Vec<Value>> {
    vec![
        super::domain_layout(
            "cardinal_number",
            13_307,
            4,
            true,
            "N",
            13_306,
            "i",
            "p",
            0,
            23,
            -1,
        ),
        super::domain_layout(
            "character_data",
            13_310,
            -1,
            false,
            "S",
            13_309,
            "i",
            "x",
            950,
            1043,
            -1,
        ),
        super::domain_layout(
            "sql_identifier",
            13_312,
            64,
            false,
            "S",
            13_311,
            "c",
            "p",
            950,
            19,
            -1,
        ),
        super::domain_layout(
            "time_stamp",
            13_318,
            8,
            true,
            "D",
            13_317,
            "d",
            "p",
            0,
            1184,
            2,
        ),
        super::domain_layout(
            "yes_or_no",
            13_320,
            -1,
            false,
            "S",
            13_319,
            "i",
            "x",
            950,
            1043,
            7,
        ),
    ]
}

#[test]
fn postgresql_18_builtin_function_catalog_preserves_overloads_and_metadata() {
    let engine = Engine::new();
    let routines = engine
        .sql(
            "SELECT oid, proname, prokind, proisstrict, proleakproof, provolatile, \
                    proparallel, pronargs, pronargdefaults, prorettype, proargtypes, proargnames, prosrc \
             FROM pg_catalog.pg_proc \
             WHERE oid IN (2311, 2321, 3062, 3261, 6330, 6331, 6332, 6333, 6342, 6343, 6364, 6382, 6383, 6389, 6390, 6429, 6430) \
             ORDER BY oid",
            &[],
        )
        .unwrap();
    assert_eq!(routines.rows.len(), 17);
    let row = |oid: i64| {
        routines
            .rows
            .iter()
            .find(|row| row["oid"] == Value::Int(oid))
            .unwrap_or_else(|| panic!("missing pg_proc row {oid}"))
    };
    assert_eq!(row(3261)["pronargs"], Value::Int(2));
    assert_eq!(row(3261)["pronargdefaults"], Value::Int(1));
    assert_eq!(row(3261)["prorettype"], Value::Int(114));
    assert_eq!(
        row(3261)["proargnames"],
        Value::Array(
            uqa_core::ArrayValue::try_new(vec![
                Value::Str("target".into()),
                Value::Str("strip_in_arrays".into()),
            ])
            .expect("flat pg_proc argument-name array")
        )
    );
    assert_md5_routines(&engine, &routines);
    assert_reverse_routines(&engine, &routines);
    for (oid, argument_type, source) in [
        (6330, 23, "to_bin32"),
        (6331, 20, "to_bin64"),
        (6332, 23, "to_oct32"),
        (6333, 20, "to_oct64"),
    ] {
        assert_eq!(row(oid)["prorettype"], Value::Int(25));
        assert_eq!(
            row(oid)["proargtypes"],
            Value::List(vec![Value::Int(argument_type)])
        );
        assert_eq!(row(oid)["proisstrict"], Value::Bool(true));
        assert_eq!(row(oid)["provolatile"], Value::Str("i".into()));
        assert_eq!(row(oid)["proleakproof"], Value::Bool(false));
        assert_eq!(row(oid)["prosrc"], Value::Str(source.into()));
    }
    assert_eq!(row(6342)["prorettype"], Value::Int(1184));
    assert_eq!(
        row(6342)["proargtypes"],
        Value::List(vec![Value::Int(2950)])
    );
    assert_eq!(row(6342)["proisstrict"], Value::Bool(true));
    assert_eq!(row(6342)["provolatile"], Value::Str("i".into()));
    assert_eq!(
        row(6342)["prosrc"],
        Value::Str("uuid_extract_timestamp".into())
    );
    assert_eq!(row(6343)["prorettype"], Value::Int(21));
    assert_eq!(
        row(6343)["prosrc"],
        Value::Str("uuid_extract_version".into())
    );
    assert_eq!(row(6364)["proleakproof"], Value::Bool(true));
    assert_eq!(row(6383)["prosrc"], Value::Str("dgamma".into()));
    assert_eq!(
        row(6389)["proargtypes"],
        Value::List(vec![Value::Int(2277), Value::Int(16)])
    );
    assert_eq!(row(6390)["pronargs"], Value::Int(3));
    assert_eq!(row(6429)["provolatile"], Value::Str("v".into()));
    assert_eq!(row(6430)["prorettype"], Value::Int(2950));

    assert_uuidv7_information_schema(&engine);
    assert_integer_base_conversion_routines(&engine);
    assert_array_transform_routines(&engine);
    assert_random_range_pg_proc(&engine);
    assert_random_range_routines(&engine);
    assert_uuid_extraction_routines(&engine);
}

fn assert_md5_routines(engine: &Engine, routines: &SQLResult) {
    let row = |oid: i64| {
        routines
            .rows
            .iter()
            .find(|row| row["oid"] == Value::Int(oid))
            .unwrap_or_else(|| panic!("missing pg_proc row {oid}"))
    };
    for (oid, argument_type, source) in [(2311, 25, "md5_text"), (2321, 17, "md5_bytea")] {
        assert_eq!(row(oid)["prokind"], Value::Str("f".into()));
        assert_eq!(row(oid)["prorettype"], Value::Int(25));
        assert_eq!(
            row(oid)["proargtypes"],
            Value::List(vec![Value::Int(argument_type)])
        );
        assert_eq!(row(oid)["pronargs"], Value::Int(1));
        assert_eq!(row(oid)["pronargdefaults"], Value::Int(0));
        assert_eq!(row(oid)["proargnames"], Value::Null);
        assert_eq!(row(oid)["proisstrict"], Value::Bool(true));
        assert_eq!(row(oid)["provolatile"], Value::Str("i".into()));
        assert_eq!(row(oid)["proparallel"], Value::Str("s".into()));
        assert_eq!(row(oid)["proleakproof"], Value::Bool(true));
        assert_eq!(row(oid)["prosrc"], Value::Str(source.into()));
    }
    let vector_text = engine
        .sql(
            "SELECT proargtypes::text AS args FROM pg_catalog.pg_proc \
             WHERE oid IN (2311, 2321) ORDER BY oid",
            &[],
        )
        .unwrap();
    assert_eq!(
        vector_text
            .rows
            .iter()
            .map(|row| row["args"].clone())
            .collect::<Vec<_>>(),
        vec![Value::Str("25".into()), Value::Str("17".into())]
    );
}

fn assert_reverse_routines(engine: &Engine, routines: &SQLResult) {
    let proargtypes = routines
        .columns
        .iter()
        .position(|column| column == "proargtypes")
        .expect("proargtypes projection");
    assert_eq!(
        routines.column_types[proargtypes],
        Some(uqa_sql::ColumnType::OidVector)
    );
    let row = |oid: i64| {
        routines
            .rows
            .iter()
            .find(|row| row["oid"] == Value::Int(oid))
            .unwrap_or_else(|| panic!("missing pg_proc row {oid}"))
    };
    for (oid, argument_type, source) in [(3062, 25, "text_reverse"), (6382, 17, "bytea_reverse")] {
        assert_eq!(row(oid)["prorettype"], Value::Int(argument_type));
        assert_eq!(
            row(oid)["proargtypes"],
            Value::List(vec![Value::Int(argument_type)])
        );
        assert_eq!(row(oid)["pronargs"], Value::Int(1));
        assert_eq!(row(oid)["pronargdefaults"], Value::Int(0));
        assert_eq!(row(oid)["proargnames"], Value::Null);
        assert_eq!(row(oid)["proisstrict"], Value::Bool(true));
        assert_eq!(row(oid)["provolatile"], Value::Str("i".into()));
        assert_eq!(row(oid)["proparallel"], Value::Str("s".into()));
        assert_eq!(row(oid)["proleakproof"], Value::Bool(false));
        assert_eq!(row(oid)["prosrc"], Value::Str(source.into()));
    }
    let vector_text = engine
        .sql(
            "SELECT proargtypes::text AS args FROM pg_catalog.pg_proc \
             WHERE oid IN (3062, 6382) ORDER BY oid",
            &[],
        )
        .unwrap();
    assert_eq!(
        vector_text
            .rows
            .iter()
            .map(|row| row["args"].clone())
            .collect::<Vec<_>>(),
        vec![Value::Str("25".into()), Value::Str("17".into())]
    );
}

fn assert_uuidv7_information_schema(engine: &Engine) {
    let routines = engine
        .sql(
            "SELECT specific_name, data_type, is_deterministic, external_language \
             FROM information_schema.routines \
             WHERE routine_name = 'uuidv7' \
             ORDER BY specific_name",
            &[],
        )
        .unwrap();
    assert_eq!(routines.rows.len(), 2);
    assert_eq!(
        routines.rows[0]["specific_name"],
        Value::Str("uuidv7_6429".into())
    );
    assert_eq!(
        routines.rows[1]["specific_name"],
        Value::Str("uuidv7_6430".into())
    );
    for row in routines.rows {
        assert_eq!(row["data_type"], Value::Str("uuid".into()));
        assert_eq!(row["is_deterministic"], Value::Str("NO".into()));
        assert_eq!(row["external_language"], Value::Str("INTERNAL".into()));
    }
}

fn assert_array_transform_routines(engine: &Engine) {
    let routines = engine
        .sql(
            "SELECT oid, proisstrict, proleakproof, provolatile, proparallel, \
                    prorettype, proargtypes, proargnames, prosrc \
             FROM pg_catalog.pg_proc WHERE oid IN (6381, 6388, 6389, 6390) ORDER BY oid",
            &[],
        )
        .unwrap();
    assert_eq!(routines.rows.len(), 4);
    for (row, (argument_types, argument_names, source)) in routines.rows.iter().zip([
        (vec![2277], Value::Null, "array_reverse"),
        (vec![2277], Value::Null, "array_sort"),
        (
            vec![2277, 16],
            array(vec![
                Value::Str("array".into()),
                Value::Str("descending".into()),
            ]),
            "array_sort_order",
        ),
        (
            vec![2277, 16, 16],
            array(vec![
                Value::Str("array".into()),
                Value::Str("descending".into()),
                Value::Str("nulls_first".into()),
            ]),
            "array_sort_order_nulls_first",
        ),
    ]) {
        assert_eq!(row["prorettype"], Value::Int(2277));
        assert_eq!(
            row["proargtypes"],
            Value::List(argument_types.into_iter().map(Value::Int).collect())
        );
        assert_eq!(row["proargnames"], argument_names);
        assert_eq!(row["proisstrict"], Value::Bool(true));
        assert_eq!(row["provolatile"], Value::Str("i".into()));
        assert_eq!(row["proparallel"], Value::Str("s".into()));
        assert_eq!(row["proleakproof"], Value::Bool(false));
        assert_eq!(row["prosrc"], Value::Str(source.into()));
    }

    let information_schema = engine
        .sql(
            "SELECT routine_name, data_type, is_deterministic, external_language \
             FROM information_schema.routines \
             WHERE routine_name IN ('array_reverse', 'array_sort') \
             ORDER BY specific_name",
            &[],
        )
        .unwrap();
    assert_eq!(information_schema.rows.len(), 4);
    for row in information_schema.rows {
        assert_eq!(row["data_type"], Value::Str("anyarray".into()));
        assert_eq!(row["is_deterministic"], Value::Str("YES".into()));
        assert_eq!(row["external_language"], Value::Str("INTERNAL".into()));
    }
}

fn assert_random_range_pg_proc(engine: &Engine) {
    let routines = engine
        .sql(
            "SELECT oid, proisstrict, proleakproof, provolatile, proparallel, \
                    prorettype, proargtypes, proargnames, prosrc \
             FROM pg_catalog.pg_proc WHERE oid IN (6339, 6340, 6341) ORDER BY oid",
            &[],
        )
        .unwrap();
    assert_eq!(routines.rows.len(), 3);
    for (row, (argument_type, source)) in routines.rows.iter().zip([
        (23, "int4random"),
        (20, "int8random"),
        (1700, "numeric_random"),
    ]) {
        assert_eq!(row["prorettype"], Value::Int(argument_type));
        assert_eq!(
            row["proargtypes"],
            Value::List(vec![Value::Int(argument_type), Value::Int(argument_type)])
        );
        assert_eq!(
            row["proargnames"],
            array(vec![Value::Str("min".into()), Value::Str("max".into())])
        );
        assert_eq!(row["proisstrict"], Value::Bool(true));
        assert_eq!(row["provolatile"], Value::Str("v".into()));
        assert_eq!(row["proparallel"], Value::Str("r".into()));
        assert_eq!(row["proleakproof"], Value::Bool(false));
        assert_eq!(row["prosrc"], Value::Str(source.into()));
    }
}

fn assert_integer_base_conversion_routines(engine: &Engine) {
    let routines = engine
        .sql(
            "SELECT routine_name, data_type, is_deterministic, external_language \
             FROM information_schema.routines \
             WHERE routine_name IN ('to_bin', 'to_oct') \
             ORDER BY routine_name, specific_name",
            &[],
        )
        .unwrap();
    assert_eq!(routines.rows.len(), 4);
    for (index, expected_name) in ["to_bin", "to_bin", "to_oct", "to_oct"].iter().enumerate() {
        assert_eq!(
            routines.rows[index]["routine_name"],
            Value::Str((*expected_name).into())
        );
        assert_eq!(routines.rows[index]["data_type"], Value::Str("text".into()));
        assert_eq!(
            routines.rows[index]["is_deterministic"],
            Value::Str("YES".into())
        );
        assert_eq!(
            routines.rows[index]["external_language"],
            Value::Str("INTERNAL".into())
        );
    }
}

fn assert_random_range_routines(engine: &Engine) {
    let routines = engine
        .sql(
            "SELECT specific_name, data_type, is_deterministic, external_language \
             FROM information_schema.routines \
             WHERE specific_name IN ('random_6339', 'random_6340', 'random_6341') \
             ORDER BY specific_name",
            &[],
        )
        .unwrap();
    assert_eq!(routines.rows.len(), 3);
    for (index, expected_type) in ["integer", "bigint", "numeric"].iter().enumerate() {
        assert_eq!(
            routines.rows[index]["data_type"],
            Value::Str((*expected_type).into())
        );
        assert_eq!(
            routines.rows[index]["is_deterministic"],
            Value::Str("NO".into())
        );
        assert_eq!(
            routines.rows[index]["external_language"],
            Value::Str("INTERNAL".into())
        );
    }
}

fn assert_uuid_extraction_routines(engine: &Engine) {
    let extraction_routines = engine
        .sql(
            "SELECT routine_name, data_type, is_deterministic, external_language \
             FROM information_schema.routines \
             WHERE routine_name IN ('uuid_extract_timestamp', 'uuid_extract_version') \
             ORDER BY routine_name",
            &[],
        )
        .unwrap();
    assert_eq!(extraction_routines.rows.len(), 2);
    assert_eq!(
        extraction_routines.rows[0]["routine_name"],
        Value::Str("uuid_extract_timestamp".into())
    );
    assert_eq!(
        extraction_routines.rows[0]["data_type"],
        Value::Str("timestamp with time zone".into())
    );
    assert_eq!(
        extraction_routines.rows[1]["routine_name"],
        Value::Str("uuid_extract_version".into())
    );
    assert_eq!(
        extraction_routines.rows[1]["data_type"],
        Value::Str("smallint".into())
    );
    for row in extraction_routines.rows {
        assert_eq!(row["is_deterministic"], Value::Str("YES".into()));
        assert_eq!(row["external_language"], Value::Str("INTERNAL".into()));
    }
}

#[test]
fn postgresql_18_user_routine_catalog_preserves_argument_modes_and_type_oids() {
    let engine = Engine::new();
    for ddl in [
        "CREATE FUNCTION cat_plain(integer) RETURNS integer AS $$ BEGIN RETURN 1; END; $$ LANGUAGE plpgsql",
        "CREATE FUNCTION cat_out(a integer, OUT x integer, OUT y text) AS $$ BEGIN x := a; y := 'x'; END; $$ LANGUAGE plpgsql",
        "CREATE FUNCTION cat_table(a integer) RETURNS TABLE(x integer, y text) AS $$ BEGIN RETURN QUERY SELECT a, 'x'; END; $$ LANGUAGE plpgsql",
        "CREATE FUNCTION cat_arrays(integer[]) RETURNS text[] AS $$ BEGIN RETURN ARRAY['x']; END; $$ LANGUAGE plpgsql",
        "CREATE PROCEDURE cat_proc(IN a integer, OUT y text) AS $$ BEGIN y := 'x'; END; $$ LANGUAGE plpgsql",
    ] {
        engine
            .sql(ddl, &[])
            .unwrap_or_else(|error| panic!("{ddl}: {error}"));
    }

    let result = engine
        .sql(
            "SELECT proname, prokind, pronargs, prorettype, proretset, proargtypes, \
                    proallargtypes, proargmodes, proargnames \
             FROM pg_catalog.pg_proc \
             WHERE proname IN ('cat_plain', 'cat_out', 'cat_table', 'cat_arrays', 'cat_proc')",
            &[],
        )
        .unwrap();
    let row = |name: &str| {
        result
            .rows
            .iter()
            .find(|row| row["proname"] == Value::Str(name.into()))
            .unwrap_or_else(|| panic!("missing pg_proc row {name}"))
    };

    assert_eq!(row("cat_plain")["pronargs"], Value::Int(1));
    assert_eq!(row("cat_plain")["prorettype"], Value::Int(23));
    assert_eq!(
        row("cat_plain")["proargtypes"],
        Value::List(vec![Value::Int(23)])
    );
    assert_eq!(row("cat_plain")["proallargtypes"], Value::Null);
    assert_eq!(row("cat_plain")["proargmodes"], Value::Null);
    assert_eq!(row("cat_plain")["proargnames"], Value::Null);

    assert_eq!(row("cat_out")["prorettype"], Value::Int(2249));
    assert_eq!(
        row("cat_out")["proargtypes"],
        Value::List(vec![Value::Int(23)])
    );
    assert_eq!(
        row("cat_out")["proallargtypes"],
        array(vec![Value::Int(23), Value::Int(23), Value::Int(25)])
    );
    assert_eq!(
        row("cat_out")["proargmodes"],
        array(vec![
            Value::Str("i".into()),
            Value::Str("o".into()),
            Value::Str("o".into())
        ])
    );
    assert_eq!(
        row("cat_out")["proargnames"],
        array(vec![
            Value::Str("a".into()),
            Value::Str("x".into()),
            Value::Str("y".into())
        ])
    );

    assert_eq!(row("cat_table")["prorettype"], Value::Int(2249));
    assert_eq!(row("cat_table")["proretset"], Value::Bool(true));
    assert_eq!(
        row("cat_table")["proargmodes"],
        array(vec![
            Value::Str("i".into()),
            Value::Str("t".into()),
            Value::Str("t".into())
        ])
    );

    assert_eq!(row("cat_arrays")["prorettype"], Value::Int(1009));
    assert_eq!(
        row("cat_arrays")["proargtypes"],
        Value::List(vec![Value::Int(1007)])
    );

    assert_eq!(row("cat_proc")["prokind"], Value::Str("p".into()));
    assert_eq!(row("cat_proc")["pronargs"], Value::Int(1));
    assert_eq!(row("cat_proc")["prorettype"], Value::Int(2249));
}

#[test]
fn postgresql_18_database_catalog_matches_builtin_unicode_behavior() {
    let engine = Engine::new();
    let database = engine
        .sql(
            "SELECT datlocprovider, datcollate, datctype, datlocale, daticurules, \
                    datcollversion, dathasloginevt \
             FROM pg_catalog.pg_database WHERE datname = 'uqa'",
            &[],
        )
        .unwrap();
    assert_eq!(database.rows.len(), 1);
    assert_eq!(database.rows[0]["datlocprovider"], Value::Str("b".into()));
    assert_eq!(database.rows[0]["datcollate"], Value::Str("C".into()));
    assert_eq!(database.rows[0]["datctype"], Value::Str("C".into()));
    assert_eq!(
        database.rows[0]["datlocale"],
        Value::Str("PG_UNICODE_FAST".into())
    );
    assert_eq!(database.rows[0]["daticurules"], Value::Null);
    assert_eq!(database.rows[0]["datcollversion"], Value::Str("1".into()));
    assert_eq!(database.rows[0]["dathasloginevt"], Value::Bool(false));

    let folded = engine
        .sql("SELECT casefold('Straße') AS folded", &[])
        .unwrap();
    assert_eq!(folded.rows[0]["folded"], Value::Str("strasse".into()));
}
