//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn pg_catalog_type_storage_metadata_matches_postgresql_18() {
    let eng = Engine::new();
    eng.sql(
        "CREATE TABLE type_layouts (
            small_value SMALLINT,
            big_value BIGINT,
            boolean_value BOOLEAN,
            text_value TEXT,
            name_value NAME,
            uuid_value UUID,
            real_value REAL,
            double_value DOUBLE PRECISION,
            interval_value INTERVAL,
            timetz_value TIME WITH TIME ZONE,
            numeric_value NUMERIC,
            big_array BIGINT[]
        )",
        &[],
    )
    .unwrap();

    let attributes = eng
        .sql(
            "SELECT attname, attlen, attbyval, attalign, attstorage
             FROM pg_catalog.pg_attribute
             WHERE attrelid = (
                 SELECT oid FROM pg_catalog.pg_class WHERE relname = 'type_layouts'
             )
             ORDER BY attnum",
            &[],
        )
        .unwrap();
    let layouts = attributes
        .rows
        .iter()
        .map(|row| {
            (
                row["attname"].clone(),
                row["attlen"].clone(),
                row["attbyval"].clone(),
                row["attalign"].clone(),
                row["attstorage"].clone(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        layouts,
        vec![
            layout("small_value", 2, true, "s", "p"),
            layout("big_value", 8, true, "d", "p"),
            layout("boolean_value", 1, true, "c", "p"),
            layout("text_value", -1, false, "i", "x"),
            layout("name_value", 64, false, "c", "p"),
            layout("uuid_value", 16, false, "c", "p"),
            layout("real_value", 4, true, "i", "p"),
            layout("double_value", 8, true, "d", "p"),
            layout("interval_value", 16, false, "d", "p"),
            layout("timetz_value", 12, false, "d", "p"),
            layout("numeric_value", -1, false, "i", "m"),
            layout("big_array", -1, false, "d", "x"),
        ]
    );
}

#[test]
fn pg_catalog_scalar_and_array_type_storage_metadata_matches_postgresql_18() {
    let eng = Engine::new();
    let types = eng
        .sql(
            "SELECT typname, typlen, typbyval, typalign, typstorage, typelem, typarray
             FROM pg_catalog.pg_type
             WHERE typname IN ('int8', '_int8', 'timetz')
             ORDER BY oid",
            &[],
        )
        .unwrap();
    assert_eq!(types.rows.len(), 3);
    assert_eq!(types.rows[0]["typname"], Value::Str("int8".into()));
    assert_eq!(types.rows[0]["typarray"], Value::Int(1016));
    assert_eq!(types.rows[1]["typname"], Value::Str("_int8".into()));
    assert_eq!(types.rows[1]["typelem"], Value::Int(20));
    assert_eq!(types.rows[1]["typalign"], Value::Str("d".into()));
    assert_eq!(types.rows[2]["typname"], Value::Str("timetz".into()));
    assert_eq!(types.rows[2]["typlen"], Value::Int(12));
}

#[test]
fn pg_catalog_system_type_storage_metadata_matches_postgresql_18() {
    let eng = Engine::new();
    let system_types = eng
        .sql(
            "SELECT typname, typlen, typbyval, typtype, typcategory, typispreferred,
                    typalign, typstorage, typelem, typarray, typsubscript, typcollation
             FROM pg_catalog.pg_type
             WHERE typname IN ('char', 'int2vector', 'regproc', 'oid', 'xid',
                               'oidvector', 'pg_node_tree', 'aclitem', 'regclass', 'regtype', 'anyarray')
             ORDER BY oid",
            &[],
        )
        .unwrap();
    let system_layouts = system_types
        .rows
        .iter()
        .map(pg_type_layout)
        .collect::<Vec<_>>();
    assert_eq!(
        system_layouts,
        vec![
            type_layout("char", 1, true, "b", "Z", false, "c", "p", 0, 1002, "-", 0),
            type_layout(
                "int2vector",
                -1,
                false,
                "b",
                "A",
                false,
                "i",
                "p",
                21,
                1006,
                "array_subscript_handler",
                0,
            ),
            type_layout("regproc", 4, true, "b", "N", false, "i", "p", 0, 1008, "-", 0),
            type_layout("oid", 4, true, "b", "N", true, "i", "p", 0, 1028, "-", 0),
            type_layout("xid", 4, true, "b", "U", false, "i", "p", 0, 1011, "-", 0),
            type_layout(
                "oidvector",
                -1,
                false,
                "b",
                "A",
                false,
                "i",
                "p",
                26,
                1013,
                "array_subscript_handler",
                0,
            ),
            type_layout(
                "pg_node_tree",
                -1,
                false,
                "b",
                "Z",
                false,
                "i",
                "x",
                0,
                0,
                "-",
                100,
            ),
            type_layout("aclitem", 16, false, "b", "U", false, "d", "p", 0, 1034, "-", 0,),
            type_layout("regclass", 4, true, "b", "N", false, "i", "p", 0, 2210, "-", 0,),
            type_layout("regtype", 4, true, "b", "N", false, "i", "p", 0, 2211, "-", 0,),
            type_layout("anyarray", -1, false, "p", "P", false, "d", "x", 0, 0, "-", 0,),
        ]
    );
}

#[test]
fn pg_catalog_system_type_arrays_match_postgresql_18() {
    let eng = Engine::new();
    let system_arrays = eng
        .sql(
            "SELECT base.typname AS base_name, array_type.typname AS array_name, array_type.typelem
             FROM pg_catalog.pg_type AS base
             JOIN pg_catalog.pg_type AS array_type ON array_type.oid = base.typarray
             WHERE base.typname IN ('char', 'int2vector', 'regproc', 'oid', 'xid',
                                    'oidvector', 'aclitem', 'regclass', 'regtype')
             ORDER BY base.oid",
            &[],
        )
        .unwrap();
    assert_eq!(system_arrays.rows.len(), 9);
    assert_eq!(
        system_arrays.rows[0]["array_name"],
        Value::Str("_char".into())
    );
    assert_eq!(system_arrays.rows[0]["typelem"], Value::Int(18));
    assert_eq!(
        system_arrays.rows[7]["array_name"],
        Value::Str("_regclass".into())
    );
    assert_eq!(system_arrays.rows[7]["typelem"], Value::Int(2205));
    assert_eq!(
        system_arrays.rows[8]["array_name"],
        Value::Str("_regtype".into())
    );
    assert_eq!(system_arrays.rows[8]["typelem"], Value::Int(2206));
}

#[test]
fn information_schema_domain_storage_metadata_matches_postgresql_18() {
    let eng = Engine::new();
    let namespace = eng
        .sql(
            "SELECT oid FROM pg_catalog.pg_namespace WHERE nspname = 'information_schema'",
            &[],
        )
        .unwrap();
    assert_eq!(namespace.rows[0]["oid"], Value::Int(13_293));

    let domains = eng
        .sql(
            "SELECT typname, oid, typlen, typbyval, typcategory, typarray,
                    typalign, typstorage, typcollation, typbasetype, typtypmod
             FROM pg_catalog.pg_type
             WHERE typnamespace = 13293 AND typtype = 'd'
             ORDER BY oid",
            &[],
        )
        .unwrap();
    let domain_layouts = domains
        .rows
        .iter()
        .map(|row| {
            [
                "typname",
                "oid",
                "typlen",
                "typbyval",
                "typcategory",
                "typarray",
                "typalign",
                "typstorage",
                "typcollation",
                "typbasetype",
                "typtypmod",
            ]
            .into_iter()
            .map(|column| row[column].clone())
            .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        domain_layouts,
        pg18_routines::expected_information_schema_domain_layouts()
    );
}

#[test]
fn information_schema_domain_arrays_match_postgresql_18() {
    let eng = Engine::new();
    let domain_arrays = eng
        .sql(
            "SELECT domain_type.typname AS domain_name, array_type.typname AS array_name,
                    array_type.oid AS array_oid, array_type.typelem
             FROM pg_catalog.pg_type AS domain_type
             JOIN pg_catalog.pg_type AS array_type ON array_type.oid = domain_type.typarray
             WHERE domain_type.typnamespace = 13293 AND domain_type.typtype = 'd'
             ORDER BY domain_type.oid",
            &[],
        )
        .unwrap();
    assert_eq!(domain_arrays.rows.len(), 5);
    assert_eq!(
        domain_arrays.rows[0]["array_name"],
        Value::Str("_cardinal_number".into())
    );
    assert_eq!(domain_arrays.rows[0]["array_oid"], Value::Int(13_306));
    assert_eq!(domain_arrays.rows[0]["typelem"], Value::Int(13_307));
    assert_eq!(
        domain_arrays.rows[4]["array_name"],
        Value::Str("_yes_or_no".into())
    );
    assert_eq!(domain_arrays.rows[4]["array_oid"], Value::Int(13_319));
    assert_eq!(domain_arrays.rows[4]["typelem"], Value::Int(13_320));
}

fn layout(
    name: &str,
    len: i64,
    by_value: bool,
    align: &str,
    storage: &str,
) -> (Value, Value, Value, Value, Value) {
    (
        Value::Str(name.into()),
        Value::Int(len),
        Value::Bool(by_value),
        Value::Str(align.into()),
        Value::Str(storage.into()),
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "names every fixture field explicitly"
)]
fn type_layout(
    name: &str,
    len: i64,
    by_value: bool,
    kind: &str,
    category: &str,
    preferred: bool,
    align: &str,
    storage: &str,
    element_oid: i64,
    array_oid: i64,
    subscript: &str,
    collation_oid: i64,
) -> Vec<Value> {
    vec![
        Value::Str(name.into()),
        Value::Int(len),
        Value::Bool(by_value),
        Value::Str(kind.into()),
        Value::Str(category.into()),
        Value::Bool(preferred),
        Value::Str(align.into()),
        Value::Str(storage.into()),
        Value::Int(element_oid),
        Value::Int(array_oid),
        Value::Str(subscript.into()),
        Value::Int(collation_oid),
    ]
}

fn pg_type_layout(row: &ResultRow) -> Vec<Value> {
    [
        "typname",
        "typlen",
        "typbyval",
        "typtype",
        "typcategory",
        "typispreferred",
        "typalign",
        "typstorage",
        "typelem",
        "typarray",
        "typsubscript",
        "typcollation",
    ]
    .into_iter()
    .map(|column| row[column].clone())
    .collect()
}

#[expect(
    clippy::too_many_arguments,
    reason = "names every fixture field explicitly"
)]
pub(super) fn domain_layout(
    name: &str,
    oid: i64,
    len: i64,
    by_value: bool,
    category: &str,
    array_oid: i64,
    align: &str,
    storage: &str,
    collation_oid: i64,
    base_oid: i64,
    type_modifier: i64,
) -> Vec<Value> {
    vec![
        Value::Str(name.into()),
        Value::Int(oid),
        Value::Int(len),
        Value::Bool(by_value),
        Value::Str(category.into()),
        Value::Int(array_oid),
        Value::Str(align.into()),
        Value::Str(storage.into()),
        Value::Int(collation_oid),
        Value::Int(base_oid),
        Value::Int(type_modifier),
    ]
}

#[test]
fn postgresql_18_type_catalog_preserves_io_routines_and_pseudo_types() {
    let eng = Engine::new();

    let pseudo_types = eng
        .sql(
            "SELECT oid, typname, typnamespace, typowner, typlen, typbyval, typtype,
                    typcategory, typispreferred, typisdefined, typdelim, typrelid,
                    typsubscript, typelem, typarray, typinput, typoutput, typreceive,
                    typsend, typmodin, typmodout, typanalyze, typalign, typstorage,
                    typnotnull, typbasetype, typtypmod, typndims, typcollation
             FROM pg_catalog.pg_type
             WHERE oid IN (2249, 2278, 2287)
             ORDER BY oid",
            &[],
        )
        .unwrap();
    assert_eq!(
        pseudo_types
            .rows
            .iter()
            .map(pg_type_full_layout)
            .collect::<Vec<_>>(),
        vec![
            pseudo_type_layout(
                2249, "record", -1, false, 0, "-", 0, 2287, 2290, 2291, 2402, 2403, 0, "d", "x",
            ),
            pseudo_type_layout(
                2278, "void", 4, true, 0, "-", 0, 0, 2298, 2299, 3120, 3121, 0, "i", "p",
            ),
            pseudo_type_layout(
                2287,
                "_record",
                -1,
                false,
                0,
                "array_subscript_handler",
                2249,
                0,
                750,
                751,
                2400,
                2401,
                3816,
                "d",
                "x",
            ),
        ]
    );

    let routine_types = eng
        .sql(
            "SELECT typname, typinput, typoutput, typreceive, typsend,
                    typmodin, typmodout, typanalyze
             FROM pg_catalog.pg_type
             WHERE typname IN (
                 'bool', 'int4', 'oid', 'regclass', 'bpchar', 'timestamptz', 'numeric', 'jsonb',
                 'aclitem', '_int4', '_bpchar', '_numeric', '_aclitem',
                 'cardinal_number', 'sql_identifier',
                 '_cardinal_number', '_sql_identifier'
             )",
            &[],
        )
        .unwrap();
    for (name, expected) in [
        ("bool", [1242, 1243, 2436, 2437, 0, 0, 0]),
        ("int4", [42, 43, 2406, 2407, 0, 0, 0]),
        ("oid", [1798, 1799, 2418, 2419, 0, 0, 0]),
        ("regclass", [2218, 2219, 2452, 2453, 0, 0, 0]),
        ("bpchar", [1044, 1045, 2430, 2431, 2913, 2914, 0]),
        ("timestamptz", [1150, 1151, 2476, 2477, 2907, 2908, 0]),
        ("numeric", [1701, 1702, 2460, 2461, 2917, 2918, 0]),
        ("jsonb", [3806, 3804, 3805, 3803, 0, 0, 0]),
        ("aclitem", [1031, 1032, 0, 0, 0, 0, 0]),
        ("_int4", [750, 751, 2400, 2401, 0, 0, 3816]),
        ("_bpchar", [750, 751, 2400, 2401, 2913, 2914, 3816]),
        ("_numeric", [750, 751, 2400, 2401, 2917, 2918, 3816]),
        ("_aclitem", [750, 751, 2400, 2401, 0, 0, 3816]),
        ("cardinal_number", [2597, 43, 2598, 2407, 0, 0, 0]),
        ("sql_identifier", [2597, 35, 2598, 2423, 0, 0, 0]),
        ("_cardinal_number", [750, 751, 2400, 2401, 0, 0, 3816]),
        ("_sql_identifier", [750, 751, 2400, 2401, 0, 0, 3816]),
    ] {
        let row = routine_types
            .rows
            .iter()
            .find(|row| row["typname"] == Value::Str(name.into()))
            .unwrap_or_else(|| panic!("missing PostgreSQL 18 type {name}"));
        assert_eq!(pg_type_routine_layout(row), expected, "type {name}");
    }
}

fn pg_type_full_layout(row: &ResultRow) -> Vec<Value> {
    [
        "oid",
        "typname",
        "typnamespace",
        "typowner",
        "typlen",
        "typbyval",
        "typtype",
        "typcategory",
        "typispreferred",
        "typisdefined",
        "typdelim",
        "typrelid",
        "typsubscript",
        "typelem",
        "typarray",
        "typinput",
        "typoutput",
        "typreceive",
        "typsend",
        "typmodin",
        "typmodout",
        "typanalyze",
        "typalign",
        "typstorage",
        "typnotnull",
        "typbasetype",
        "typtypmod",
        "typndims",
        "typcollation",
    ]
    .into_iter()
    .map(|column| row[column].clone())
    .collect()
}

#[expect(
    clippy::too_many_arguments,
    reason = "names every fixture field explicitly"
)]
fn pseudo_type_layout(
    oid: i64,
    name: &str,
    len: i64,
    by_value: bool,
    relation_oid: i64,
    subscript: &str,
    element_oid: i64,
    array_oid: i64,
    input: i64,
    output: i64,
    receive: i64,
    send: i64,
    analyze: i64,
    align: &str,
    storage: &str,
) -> Vec<Value> {
    vec![
        Value::Int(oid),
        Value::Str(name.into()),
        Value::Int(11),
        Value::Int(10),
        Value::Int(len),
        Value::Bool(by_value),
        Value::Str("p".into()),
        Value::Str("P".into()),
        Value::Bool(false),
        Value::Bool(true),
        Value::Str(",".into()),
        Value::Int(relation_oid),
        Value::Str(subscript.into()),
        Value::Int(element_oid),
        Value::Int(array_oid),
        Value::Int(input),
        Value::Int(output),
        Value::Int(receive),
        Value::Int(send),
        Value::Int(0),
        Value::Int(0),
        Value::Int(analyze),
        Value::Str(align.into()),
        Value::Str(storage.into()),
        Value::Bool(false),
        Value::Int(0),
        Value::Int(-1),
        Value::Int(0),
        Value::Int(0),
    ]
}

pub(super) fn pg_type_routine_layout(row: &ResultRow) -> [i64; 7] {
    [
        "typinput",
        "typoutput",
        "typreceive",
        "typsend",
        "typmodin",
        "typmodout",
        "typanalyze",
    ]
    .map(|column| match row[column] {
        Value::Int(value) => value,
        ref value => panic!("expected integer routine OID for {column}, got {value:?}"),
    })
}
