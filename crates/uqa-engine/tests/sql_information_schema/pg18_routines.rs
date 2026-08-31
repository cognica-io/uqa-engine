//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 built-in routine and database catalog regressions.

use uqa_core::{ArrayValue, Value};
use uqa_engine::{Engine, SQLResult};
use uqa_sql::ColumnType;

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
            "SELECT oid, proname, pronamespace, proowner, prolang, procost, prorows, provariadic, \
                    prosupport, prosupport::text AS prosupport_text, prokind, prosecdef, proleakproof, proisstrict, proretset, provolatile, \
                    proparallel, pronargs, pronargdefaults, prorettype, proargtypes, proallargtypes, \
                    proargmodes, proargnames, proargdefaults, protrftypes, prosrc, probin, prosqlbody, \
                    proconfig, proacl \
             FROM pg_catalog.pg_proc \
             WHERE oid IN (720, 1317, 1318, 1367, 1369, 1372, 1374, 1375, 1381, 1598, 1810, 1811, 2010, 2089, 2090, 2311, 2321, 3062, 3261, 3262, 3432, 6330, 6331, 6332, 6333, 6342, 6343, 6364, 6365, 6382, 6383, 6384, 6389, 6390, 6412, 6428, 6429, 6430) \
             ORDER BY oid",
            &[],
        )
        .unwrap();
    assert_eq!(routines.rows.len(), 38);
    let row = |oid: i64| {
        routines
            .rows
            .iter()
            .find(|row| row["oid"] == Value::Int(oid))
            .unwrap_or_else(|| panic!("missing pg_proc row {oid}"))
    };
    assert_json_strip_routines(&engine, &routines);
    assert_string_binary_length_routines(&engine, &routines);
    assert_checksum_routines(&engine, &routines);
    assert_gamma_routines(&engine, &routines);
    assert_md5_routines(&engine, &routines);
    assert_reverse_routines(&engine, &routines);
    for (oid, argument_type, source) in [
        (2089, 23, "to_hex32"),
        (2090, 20, "to_hex64"),
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
        assert_eq!(row(oid)["proparallel"], Value::Str("s".into()));
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
    assert_eq!(
        row(6389)["proargtypes"],
        Value::List(vec![Value::Int(2277), Value::Int(16)])
    );
    assert_eq!(row(6390)["pronargs"], Value::Int(3));
    assert_eq!(row(6429)["provolatile"], Value::Str("v".into()));
    assert_eq!(row(6430)["prorettype"], Value::Int(2950));

    assert_pg18_fixed_routine_metadata(&routines);
    assert_pg18_fixed_routine_information_schema(&engine);
    assert_uuidv7_information_schema(&engine);
    assert_integer_base_conversion_routines(&engine);
    assert_array_transform_routines(&engine);
    assert_random_range_pg_proc(&engine);
    assert_random_range_routines(&engine);
    assert_uuid_extraction_routines(&engine);
}

fn assert_pg18_fixed_routine_metadata(routines: &SQLResult) {
    let row = |oid: i64| {
        routines
            .rows
            .iter()
            .find(|row| row["oid"] == Value::Int(oid))
            .unwrap_or_else(|| panic!("missing pg_proc row {oid}"))
    };
    for (oid, name, volatility, parallel, return_type, argument_types, source) in [
        (1598, "random", "v", "r", 701, vec![], "drandom"),
        (2089, "to_hex", "i", "s", 25, vec![23], "to_hex32"),
        (2090, "to_hex", "i", "s", 25, vec![20], "to_hex64"),
        (
            3432,
            "gen_random_uuid",
            "v",
            "s",
            2950,
            vec![],
            "gen_random_uuid",
        ),
        (6412, "casefold", "i", "s", 25, vec![25], "casefold"),
        (6428, "uuidv4", "v", "s", 2950, vec![], "gen_random_uuid"),
    ] {
        assert_eq!(row(oid)["proname"], Value::Str(name.into()));
        assert_eq!(row(oid)["pronamespace"], Value::Int(11));
        assert_eq!(row(oid)["proowner"], Value::Int(10));
        assert_eq!(row(oid)["prolang"], Value::Int(12));
        assert_eq!(row(oid)["procost"], Value::Float(1.0));
        assert_eq!(row(oid)["prorows"], Value::Float(0.0));
        assert_eq!(row(oid)["provariadic"], Value::Int(0));
        assert_eq!(row(oid)["prosupport"], Value::Int(0));
        assert_eq!(row(oid)["prosupport_text"], Value::Str("-".into()));
        assert_eq!(row(oid)["prokind"], Value::Str("f".into()));
        assert_eq!(row(oid)["prosecdef"], Value::Bool(false));
        assert_eq!(row(oid)["proleakproof"], Value::Bool(false));
        assert_eq!(row(oid)["proisstrict"], Value::Bool(true));
        assert_eq!(row(oid)["proretset"], Value::Bool(false));
        assert_eq!(row(oid)["provolatile"], Value::Str(volatility.into()));
        assert_eq!(row(oid)["proparallel"], Value::Str(parallel.into()));
        assert_eq!(
            row(oid)["pronargs"],
            Value::Int(i64::try_from(argument_types.len()).expect("routine arity fits i64"))
        );
        assert_eq!(row(oid)["pronargdefaults"], Value::Int(0));
        assert_eq!(row(oid)["prorettype"], Value::Int(return_type));
        assert_eq!(
            row(oid)["proargtypes"],
            Value::List(argument_types.into_iter().map(Value::Int).collect())
        );
        assert_eq!(row(oid)["proallargtypes"], Value::Null);
        assert_eq!(row(oid)["proargmodes"], Value::Null);
        assert_eq!(row(oid)["proargnames"], Value::Null);
        assert_eq!(row(oid)["proargdefaults"], Value::Null);
        assert_eq!(row(oid)["protrftypes"], Value::Null);
        assert_eq!(row(oid)["prosrc"], Value::Str(source.into()));
        assert_eq!(row(oid)["probin"], Value::Null);
        assert_eq!(row(oid)["prosqlbody"], Value::Null);
        assert_eq!(row(oid)["proconfig"], Value::Null);
        assert_eq!(row(oid)["proacl"], Value::Null);
    }
}

fn assert_pg18_fixed_routine_information_schema(engine: &Engine) {
    let routines = engine
        .sql(
            "SELECT specific_name, routine_name, data_type, is_deterministic, external_language \
             FROM information_schema.routines \
             WHERE specific_name IN ('random_1598', 'gen_random_uuid_3432', 'casefold_6412', 'uuidv4_6428') \
             ORDER BY specific_name",
            &[],
        )
        .unwrap();
    assert_eq!(routines.rows.len(), 4);
    for (row, (specific_name, routine_name, data_type, deterministic)) in
        routines.rows.iter().zip([
            ("casefold_6412", "casefold", "text", "YES"),
            ("gen_random_uuid_3432", "gen_random_uuid", "uuid", "NO"),
            ("random_1598", "random", "double precision", "NO"),
            ("uuidv4_6428", "uuidv4", "uuid", "NO"),
        ])
    {
        assert_eq!(row["specific_name"], Value::Str(specific_name.into()));
        assert_eq!(row["routine_name"], Value::Str(routine_name.into()));
        assert_eq!(row["data_type"], Value::Str(data_type.into()));
        assert_eq!(row["is_deterministic"], Value::Str(deterministic.into()));
        assert_eq!(row["external_language"], Value::Str("INTERNAL".into()));
    }
}

fn assert_json_strip_routines(engine: &Engine, routines: &SQLResult) {
    const FALSE_NODE: &str = "({CONST :consttype 16 :consttypmod -1 :constcollid 0 :constlen 1 :constbyval true :constisnull false :location -1 :constvalue 1 [ 0 0 0 0 0 0 0 0 ]})";
    let row = |oid: i64| {
        routines
            .rows
            .iter()
            .find(|row| row["oid"] == Value::Int(oid))
            .unwrap_or_else(|| panic!("missing pg_proc row {oid}"))
    };
    for (oid, name, target_type) in [
        (3261, "json_strip_nulls", 114),
        (3262, "jsonb_strip_nulls", 3802),
    ] {
        assert_eq!(row(oid)["proname"], Value::Str(name.into()));
        assert_eq!(row(oid)["pronamespace"], Value::Int(11));
        assert_eq!(row(oid)["proowner"], Value::Int(10));
        assert_eq!(row(oid)["prolang"], Value::Int(12));
        assert_eq!(row(oid)["procost"], Value::Float(1.0));
        assert_eq!(row(oid)["prorows"], Value::Float(0.0));
        assert_eq!(row(oid)["provariadic"], Value::Int(0));
        assert_eq!(row(oid)["prosupport"], Value::Int(0));
        assert_eq!(row(oid)["prokind"], Value::Str("f".into()));
        assert_eq!(row(oid)["prosecdef"], Value::Bool(false));
        assert_eq!(row(oid)["proleakproof"], Value::Bool(false));
        assert_eq!(row(oid)["proisstrict"], Value::Bool(true));
        assert_eq!(row(oid)["proretset"], Value::Bool(false));
        assert_eq!(row(oid)["provolatile"], Value::Str("i".into()));
        assert_eq!(row(oid)["proparallel"], Value::Str("s".into()));
        assert_eq!(row(oid)["pronargs"], Value::Int(2));
        assert_eq!(row(oid)["pronargdefaults"], Value::Int(1));
        assert_eq!(row(oid)["prorettype"], Value::Int(target_type));
        assert_eq!(
            row(oid)["proargtypes"],
            Value::List(vec![Value::Int(target_type), Value::Int(16)])
        );
        assert_eq!(row(oid)["proallargtypes"], Value::Null);
        assert_eq!(row(oid)["proargmodes"], Value::Null);
        assert_eq!(
            row(oid)["proargnames"],
            Value::Array(
                uqa_core::ArrayValue::try_new(vec![
                    Value::Str("target".into()),
                    Value::Str("strip_in_arrays".into()),
                ])
                .expect("flat pg_proc argument-name array")
            )
        );
        assert_eq!(row(oid)["proargdefaults"], Value::Str(FALSE_NODE.into()));
        assert_eq!(row(oid)["protrftypes"], Value::Null);
        assert_eq!(row(oid)["prosrc"], Value::Str(name.into()));
        assert_eq!(row(oid)["probin"], Value::Null);
        assert_eq!(row(oid)["prosqlbody"], Value::Null);
        assert_eq!(row(oid)["proconfig"], Value::Null);
        assert_eq!(row(oid)["proacl"], Value::Null);
    }
    let information_schema = engine
        .sql(
            "SELECT specific_name, routine_name, data_type, is_deterministic, external_language \
             FROM information_schema.routines \
             WHERE specific_name IN ('json_strip_nulls_3261', 'jsonb_strip_nulls_3262') \
             ORDER BY specific_name",
            &[],
        )
        .unwrap();
    assert_eq!(information_schema.rows.len(), 2);
    for (row, (specific_name, routine_name, data_type)) in information_schema.rows.iter().zip([
        ("json_strip_nulls_3261", "json_strip_nulls", "json"),
        ("jsonb_strip_nulls_3262", "jsonb_strip_nulls", "jsonb"),
    ]) {
        assert_eq!(row["specific_name"], Value::Str(specific_name.into()));
        assert_eq!(row["routine_name"], Value::Str(routine_name.into()));
        assert_eq!(row["data_type"], Value::Str(data_type.into()));
        assert_eq!(row["is_deterministic"], Value::Str("YES".into()));
        assert_eq!(row["external_language"], Value::Str("INTERNAL".into()));
    }
}

fn assert_gamma_routines(engine: &Engine, routines: &SQLResult) {
    let row = |oid: i64| {
        routines
            .rows
            .iter()
            .find(|row| row["oid"] == Value::Int(oid))
            .unwrap_or_else(|| panic!("missing pg_proc row {oid}"))
    };
    for (oid, name, source) in [(6383, "gamma", "dgamma"), (6384, "lgamma", "dlgamma")] {
        assert_eq!(row(oid)["proname"], Value::Str(name.into()));
        assert_eq!(row(oid)["pronamespace"], Value::Int(11));
        assert_eq!(row(oid)["proowner"], Value::Int(10));
        assert_eq!(row(oid)["prolang"], Value::Int(12));
        assert_eq!(row(oid)["procost"], Value::Float(1.0));
        assert_eq!(row(oid)["prorows"], Value::Float(0.0));
        assert_eq!(row(oid)["provariadic"], Value::Int(0));
        assert_eq!(row(oid)["prosupport"], Value::Int(0));
        assert_eq!(row(oid)["prokind"], Value::Str("f".into()));
        assert_eq!(row(oid)["prosecdef"], Value::Bool(false));
        assert_eq!(row(oid)["proleakproof"], Value::Bool(false));
        assert_eq!(row(oid)["proisstrict"], Value::Bool(true));
        assert_eq!(row(oid)["proretset"], Value::Bool(false));
        assert_eq!(row(oid)["provolatile"], Value::Str("i".into()));
        assert_eq!(row(oid)["proparallel"], Value::Str("s".into()));
        assert_eq!(row(oid)["pronargs"], Value::Int(1));
        assert_eq!(row(oid)["pronargdefaults"], Value::Int(0));
        assert_eq!(row(oid)["prorettype"], Value::Int(701));
        assert_eq!(row(oid)["proargtypes"], Value::List(vec![Value::Int(701)]));
        assert_eq!(row(oid)["proallargtypes"], Value::Null);
        assert_eq!(row(oid)["proargmodes"], Value::Null);
        assert_eq!(row(oid)["proargnames"], Value::Null);
        assert_eq!(row(oid)["proargdefaults"], Value::Null);
        assert_eq!(row(oid)["protrftypes"], Value::Null);
        assert_eq!(row(oid)["prosrc"], Value::Str(source.into()));
        assert_eq!(row(oid)["probin"], Value::Null);
        assert_eq!(row(oid)["prosqlbody"], Value::Null);
        assert_eq!(row(oid)["proconfig"], Value::Null);
        assert_eq!(row(oid)["proacl"], Value::Null);
    }
    let vector_text = engine
        .sql(
            "SELECT proargtypes::text AS args FROM pg_catalog.pg_proc \
             WHERE oid IN (6383, 6384) ORDER BY oid",
            &[],
        )
        .unwrap();
    assert_eq!(
        vector_text
            .rows
            .iter()
            .map(|row| row["args"].clone())
            .collect::<Vec<_>>(),
        vec![Value::Str("701".into()), Value::Str("701".into())]
    );
    let information_schema = engine
        .sql(
            "SELECT specific_name, routine_name, data_type, is_deterministic, external_language \
             FROM information_schema.routines \
             WHERE specific_name IN ('gamma_6383', 'lgamma_6384') ORDER BY specific_name",
            &[],
        )
        .unwrap();
    assert_eq!(information_schema.rows.len(), 2);
    for (row, (specific_name, routine_name)) in information_schema
        .rows
        .iter()
        .zip([("gamma_6383", "gamma"), ("lgamma_6384", "lgamma")])
    {
        assert_eq!(row["specific_name"], Value::Str(specific_name.into()));
        assert_eq!(row["routine_name"], Value::Str(routine_name.into()));
        assert_eq!(row["data_type"], Value::Str("double precision".into()));
        assert_eq!(row["is_deterministic"], Value::Str("YES".into()));
        assert_eq!(row["external_language"], Value::Str("INTERNAL".into()));
    }
}

fn assert_string_binary_length_routines(engine: &Engine, routines: &SQLResult) {
    let row = |oid: i64| {
        routines
            .rows
            .iter()
            .find(|row| row["oid"] == Value::Int(oid))
            .unwrap_or_else(|| panic!("missing pg_proc row {oid}"))
    };
    for (oid, argument_type, source) in [
        (720, 17, "byteaoctetlen"),
        (1317, 25, "textlen"),
        (1318, 1042, "bpcharlen"),
        (1367, 1042, "bpcharlen"),
        (1369, 25, "textlen"),
        (1372, 1042, "bpcharlen"),
        (1374, 25, "textoctetlen"),
        (1375, 1042, "bpcharoctetlen"),
        (1381, 25, "textlen"),
        (1810, 17, ""),
        (1811, 25, ""),
        (2010, 17, "byteaoctetlen"),
    ] {
        assert_eq!(row(oid)["prokind"], Value::Str("f".into()));
        assert_eq!(
            row(oid)["prolang"],
            Value::Int(if matches!(oid, 1810 | 1811) { 14 } else { 12 })
        );
        assert_eq!(row(oid)["prorettype"], Value::Int(23));
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
        if matches!(oid, 1810 | 1811) {
            let Value::Str(body) = &row(oid)["prosqlbody"] else {
                panic!("bit_length OID {oid} must retain its SQL body")
            };
            let function_oid = if oid == 1810 { 720 } else { 1374 };
            assert!(body.contains(&format!(":funcid {function_oid} ")));
            assert!(body.contains(":opno 514 :opfuncid 141 :opresulttype 23 "));
        } else {
            assert_eq!(row(oid)["prosqlbody"], Value::Null);
        }
    }
    let vector_text = engine
        .sql(
            "SELECT proargtypes::text AS args FROM pg_catalog.pg_proc \
             WHERE oid IN (720, 1317, 1318, 1367, 1369, 1372, 1374, 1375, 1381, 1810, 1811, 2010) ORDER BY oid",
            &[],
        )
        .unwrap();
    assert_eq!(
        vector_text
            .rows
            .iter()
            .map(|row| row["args"].clone())
            .collect::<Vec<_>>(),
        [17, 25, 1042, 1042, 25, 1042, 25, 1042, 25, 17, 25, 17]
            .into_iter()
            .map(|oid| Value::Str(oid.to_string()))
            .collect::<Vec<_>>()
    );
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

fn assert_checksum_routines(engine: &Engine, routines: &SQLResult) {
    let row = |oid: i64| {
        routines
            .rows
            .iter()
            .find(|row| row["oid"] == Value::Int(oid))
            .unwrap_or_else(|| panic!("missing pg_proc row {oid}"))
    };
    for (oid, name, source) in [
        (6364, "crc32", "crc32_bytea"),
        (6365, "crc32c", "crc32c_bytea"),
    ] {
        assert_eq!(row(oid)["proname"], Value::Str(name.into()));
        assert_eq!(row(oid)["prolang"], Value::Int(12));
        assert_eq!(row(oid)["prokind"], Value::Str("f".into()));
        assert_eq!(row(oid)["prorettype"], Value::Int(20));
        assert_eq!(row(oid)["proargtypes"], Value::List(vec![Value::Int(17)]));
        assert_eq!(row(oid)["pronargs"], Value::Int(1));
        assert_eq!(row(oid)["pronargdefaults"], Value::Int(0));
        assert_eq!(row(oid)["proargnames"], Value::Null);
        assert_eq!(row(oid)["proisstrict"], Value::Bool(true));
        assert_eq!(row(oid)["provolatile"], Value::Str("i".into()));
        assert_eq!(row(oid)["proparallel"], Value::Str("s".into()));
        assert_eq!(row(oid)["proleakproof"], Value::Bool(true));
        assert_eq!(row(oid)["prosrc"], Value::Str(source.into()));
        assert_eq!(row(oid)["prosqlbody"], Value::Null);
    }
    let information_schema = engine
        .sql(
            "SELECT specific_name, routine_name, data_type, is_deterministic, external_language \
             FROM information_schema.routines \
             WHERE specific_name IN ('crc32_6364', 'crc32c_6365') ORDER BY specific_name",
            &[],
        )
        .unwrap();
    assert_eq!(information_schema.rows.len(), 2);
    for (row, (specific_name, routine_name)) in information_schema
        .rows
        .iter()
        .zip([("crc32_6364", "crc32"), ("crc32c_6365", "crc32c")])
    {
        assert_eq!(row["specific_name"], Value::Str(specific_name.into()));
        assert_eq!(row["routine_name"], Value::Str(routine_name.into()));
        assert_eq!(row["data_type"], Value::Str("bigint".into()));
        assert_eq!(row["is_deterministic"], Value::Str("YES".into()));
        assert_eq!(row["external_language"], Value::Str("INTERNAL".into()));
    }
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
            "SELECT specific_name, routine_name, data_type, is_deterministic, external_language \
             FROM information_schema.routines \
             WHERE routine_name IN ('to_bin', 'to_hex', 'to_oct') \
             ORDER BY routine_name, specific_name",
            &[],
        )
        .unwrap();
    assert_eq!(routines.rows.len(), 6);
    for (index, (specific_name, routine_name)) in [
        ("to_bin_6330", "to_bin"),
        ("to_bin_6331", "to_bin"),
        ("to_hex_2089", "to_hex"),
        ("to_hex_2090", "to_hex"),
        ("to_oct_6332", "to_oct"),
        ("to_oct_6333", "to_oct"),
    ]
    .iter()
    .enumerate()
    {
        assert_eq!(
            routines.rows[index]["specific_name"],
            Value::Str((*specific_name).into())
        );
        assert_eq!(
            routines.rows[index]["routine_name"],
            Value::Str((*routine_name).into())
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

#[test]
fn postgresql_18_regtype_aliases_use_catalog_aware_text_output() {
    let engine = Engine::new();
    let result = engine
        .sql(
            "SELECT (0::regproc)::text AS zero_proc, (0::regclass)::text AS zero_class, \
                    (0::regnamespace)::text AS zero_namespace, (0::regtype)::text AS zero_type, \
                    (1574::regproc)::text AS sequence_proc_name, (1598::regproc)::text AS proc_name, \
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

    let sequence_routines = engine
        .sql(
            "SELECT oid, proname, prorettype, proargtypes, proisstrict, provolatile, proparallel, prosrc FROM pg_proc WHERE oid IN (1574, 1575, 1576, 1765) ORDER BY oid",
            &[],
        )
        .unwrap();
    assert_eq!(sequence_routines.rows.len(), 4);
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
