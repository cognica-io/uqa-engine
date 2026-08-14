//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 built-in routine and database catalog regressions.

use uqa_core::Value;
use uqa_engine::Engine;

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
                    pronargs, pronargdefaults, prorettype, proargtypes, proargnames, prosrc \
             FROM pg_catalog.pg_proc \
             WHERE oid IN (3261, 6364, 6383, 6389, 6390, 6429, 6430) \
             ORDER BY oid",
            &[],
        )
        .unwrap();
    assert_eq!(routines.rows.len(), 7);
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
        Value::List(vec![
            Value::Str("target".into()),
            Value::Str("strip_in_arrays".into()),
        ])
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

    let information_schema = engine
        .sql(
            "SELECT specific_name, data_type, is_deterministic, external_language \
             FROM information_schema.routines \
             WHERE routine_name = 'uuidv7' \
             ORDER BY specific_name",
            &[],
        )
        .unwrap();
    assert_eq!(information_schema.rows.len(), 2);
    assert_eq!(
        information_schema.rows[0]["specific_name"],
        Value::Str("uuidv7_6429".into())
    );
    assert_eq!(
        information_schema.rows[1]["specific_name"],
        Value::Str("uuidv7_6430".into())
    );
    for row in information_schema.rows {
        assert_eq!(row["data_type"], Value::Str("uuid".into()));
        assert_eq!(row["is_deterministic"], Value::Str("NO".into()));
        assert_eq!(row["external_language"], Value::Str("INTERNAL".into()));
    }
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
