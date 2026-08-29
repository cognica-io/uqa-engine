//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn information_schema_catalog_name_preserves_its_pg18_composite_identity() {
    let eng = Engine::new();

    let catalog_name = eng
        .sql(
            "SELECT * FROM information_schema.information_schema_catalog_name",
            &[],
        )
        .unwrap();
    assert_eq!(catalog_name.columns, ["catalog_name"]);
    assert_eq!(
        catalog_name.column_types,
        [Some(ColumnType::Domain {
            schema: "information_schema".into(),
            name: "sql_identifier".into(),
            oid: 13_312,
            base: Box::new(ColumnType::Name),
        })]
    );
    assert_eq!(catalog_name.rows.len(), 1);
    assert_eq!(
        catalog_name.rows[0]["catalog_name"],
        Value::Str("uqa".into())
    );
}

#[test]
fn information_schema_catalog_name_preserves_its_pg18_class_identity() {
    let eng = Engine::new();
    let class = eng
        .sql(
            "SELECT oid, relname, relnamespace, reltype, relowner, relpages, reltuples,
                    relallvisible, relallfrozen, relhasindex, relpersistence, relkind,
                    relnatts, relchecks, relhasrules, relhastriggers, relispopulated,
                    relreplident, relispartition
             FROM pg_catalog.pg_class
             WHERE oid = 13313",
            &[],
        )
        .unwrap();
    let class = &class.rows[0];
    for (column, expected) in [
        ("oid", Value::Int(13_313)),
        (
            "relname",
            Value::Str("information_schema_catalog_name".into()),
        ),
        ("relnamespace", Value::Int(13_293)),
        ("reltype", Value::Int(13_315)),
        ("relowner", Value::Int(10)),
        ("relpages", Value::Int(0)),
        ("reltuples", Value::Float(-1.0)),
        ("relallvisible", Value::Int(0)),
        ("relallfrozen", Value::Int(0)),
        ("relhasindex", Value::Bool(false)),
        ("relpersistence", Value::Str("p".into())),
        ("relkind", Value::Str("v".into())),
        ("relnatts", Value::Int(1)),
        ("relchecks", Value::Int(0)),
        ("relhasrules", Value::Bool(true)),
        ("relhastriggers", Value::Bool(false)),
        ("relispopulated", Value::Bool(true)),
        ("relreplident", Value::Str("n".into())),
        ("relispartition", Value::Bool(false)),
    ] {
        assert_eq!(class[column], expected, "pg_class.{column}");
    }
}

#[test]
fn information_schema_catalog_name_preserves_its_pg18_attribute_identity() {
    let eng = Engine::new();
    let attribute = eng
        .sql(
            "SELECT attrelid, attname, atttypid, attstattarget, attlen, attnum,
                    atttypmod, attndims, attbyval, attalign, attstorage, attcompression,
                    attnotnull, atthasdef, atthasmissing, attidentity, attgenerated,
                    attisdropped, attislocal, attinhcount, attcollation
             FROM pg_catalog.pg_attribute
             WHERE attrelid = 13313 AND attnum = 1",
            &[],
        )
        .unwrap();
    let attribute = &attribute.rows[0];
    for (column, expected) in [
        ("attrelid", Value::Int(13_313)),
        ("attname", Value::Str("catalog_name".into())),
        ("atttypid", Value::Int(13_312)),
        ("attstattarget", Value::Null),
        ("attlen", Value::Int(64)),
        ("attnum", Value::Int(1)),
        ("atttypmod", Value::Int(-1)),
        ("attndims", Value::Int(0)),
        ("attbyval", Value::Bool(false)),
        ("attalign", Value::Str("c".into())),
        ("attstorage", Value::Str("p".into())),
        ("attcompression", Value::Str(String::new())),
        ("attnotnull", Value::Bool(false)),
        ("atthasdef", Value::Bool(false)),
        ("atthasmissing", Value::Bool(false)),
        ("attidentity", Value::Str(String::new())),
        ("attgenerated", Value::Str(String::new())),
        ("attisdropped", Value::Bool(false)),
        ("attislocal", Value::Bool(true)),
        ("attinhcount", Value::Int(0)),
        ("attcollation", Value::Int(950)),
    ] {
        assert_eq!(attribute[column], expected, "pg_attribute.{column}");
    }
}

#[test]
fn pg_attribute_exposes_fast_defaults_and_volatile_rewrites() {
    let eng = Engine::new();
    eng.sql(
        "CREATE SEQUENCE attribute_default_sequence START 10;
         CREATE TABLE attribute_defaults (id INTEGER PRIMARY KEY);
         INSERT INTO attribute_defaults VALUES (1), (2);
         ALTER TABLE attribute_defaults ADD COLUMN fast_default INTEGER DEFAULT 7",
        &[],
    )
    .unwrap();

    let fast_attribute = eng
        .sql(
            "SELECT atthasmissing, attmissingval
             FROM pg_catalog.pg_attribute
             WHERE attrelid = 'attribute_defaults'::regclass
               AND attname = 'fast_default'",
            &[],
        )
        .unwrap();
    assert_eq!(fast_attribute.rows[0]["atthasmissing"], Value::Bool(true));
    assert_eq!(
        fast_attribute.rows[0]["attmissingval"],
        array(vec![Value::Int(7)])
    );

    eng.sql(
        "ALTER TABLE attribute_defaults ADD COLUMN volatile_default BIGINT DEFAULT nextval('attribute_default_sequence')",
        &[],
    )
    .unwrap();

    let attributes = eng
        .sql(
            "SELECT attname, atthasmissing, attmissingval
             FROM pg_catalog.pg_attribute
             WHERE attrelid = 'attribute_defaults'::regclass
               AND attname IN ('fast_default', 'volatile_default')
             ORDER BY attname",
            &[],
        )
        .unwrap();
    assert_eq!(attributes.rows.len(), 2);
    assert_eq!(
        attributes.rows[0]["attname"],
        Value::Str("fast_default".into())
    );
    assert_eq!(attributes.rows[0]["atthasmissing"], Value::Bool(false));
    assert_eq!(attributes.rows[0]["attmissingval"], Value::Null);
    assert_eq!(
        attributes.rows[1]["attname"],
        Value::Str("volatile_default".into())
    );
    assert_eq!(attributes.rows[1]["atthasmissing"], Value::Bool(false));
    assert_eq!(attributes.rows[1]["attmissingval"], Value::Null);

    let rows = eng
        .sql(
            "SELECT volatile_default FROM attribute_defaults ORDER BY id",
            &[],
        )
        .unwrap();
    assert_eq!(
        rows.rows
            .iter()
            .map(|row| row["volatile_default"].clone())
            .collect::<Vec<_>>(),
        [Value::Int(10), Value::Int(11)]
    );
}

#[test]
fn information_schema_catalog_name_preserves_its_pg18_type_identity() {
    let eng = Engine::new();
    let types = eng
        .sql(
            "SELECT oid, typname, typnamespace, typtype, typcategory, typrelid,
                    typsubscript, typelem, typarray, typinput, typoutput, typreceive,
                    typsend, typmodin, typmodout, typanalyze, typalign, typstorage
             FROM pg_catalog.pg_type
             WHERE oid IN (13314, 13315)
             ORDER BY oid",
            &[],
        )
        .unwrap();
    assert_eq!(types.rows.len(), 2);
    let array_type = &types.rows[0];
    assert_eq!(
        array_type["typname"],
        Value::Str("_information_schema_catalog_name".into())
    );
    assert_eq!(array_type["typtype"], Value::Str("b".into()));
    assert_eq!(array_type["typcategory"], Value::Str("A".into()));
    assert_eq!(array_type["typrelid"], Value::Int(0));
    assert_eq!(
        array_type["typsubscript"],
        Value::Str("array_subscript_handler".into())
    );
    assert_eq!(array_type["typelem"], Value::Int(13_315));
    assert_eq!(array_type["typarray"], Value::Int(0));
    assert_eq!(
        pg_type_routine_layout(array_type),
        [750, 751, 2400, 2401, 0, 0, 3816]
    );

    let composite_type = &types.rows[1];
    assert_eq!(
        composite_type["typname"],
        Value::Str("information_schema_catalog_name".into())
    );
    assert_eq!(composite_type["typtype"], Value::Str("c".into()));
    assert_eq!(composite_type["typcategory"], Value::Str("C".into()));
    assert_eq!(composite_type["typrelid"], Value::Int(13_313));
    assert_eq!(composite_type["typsubscript"], Value::Str("-".into()));
    assert_eq!(composite_type["typelem"], Value::Int(0));
    assert_eq!(composite_type["typarray"], Value::Int(13_314));
    assert_eq!(
        pg_type_routine_layout(composite_type),
        [2290, 2291, 2402, 2403, 0, 0, 0]
    );
}
