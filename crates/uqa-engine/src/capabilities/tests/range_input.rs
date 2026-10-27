//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Verify the Engine state adapters against actual SQL and procedural invocation; declaration parsing remains a separate owner boundary.

use super::*;
use uqa_sql::catalog::range_type::{StoredMultirange, StoredRange};

fn range() -> StoredRange {
    StoredRange {
        object_id: [11; 16],
        oid: 70_000,
        array_oid: 70_001,
        array_name: "_r".into(),
        identity: crate::RelationIdentity::new("public", "r"),
        owner: uqa_core::catalog_role::RoleIdentity::BOOTSTRAP,
        subtype: ColumnType::Integer,
        subtype_operator_class: 1978,
        collation_oid: 0,
        canonical_oid: None,
        subtype_difference_oid: None,
        usage_acl: None,
        multirange: StoredMultirange {
            object_id: [12; 16],
            oid: 70_002,
            array_oid: 70_003,
            array_name: "_rs".into(),
            identity: crate::RelationIdentity::new("public", "rs"),
        },
    }
}

#[test]
fn host_range_constructors_share_statement_effects_and_recover_native_transaction_failure() {
    use uqa_core::memory::ProductionControl;
    use uqa_sql::params::RangeParameterBounds;
    let engine = Engine::new();
    engine
        .durable
        .ranges
        .write()
        .insert("public.r".into(), range());
    engine.sql("CREATE SEQUENCE host_canonical_calls; CREATE FUNCTION host_canonical(r) RETURNS r LANGUAGE plpgsql VOLATILE AS $$ BEGIN PERFORM nextval('host_canonical_calls'); RETURN $1; END $$", &[]).unwrap();
    let result = engine
        .sql(
            "SELECT oid FROM pg_proc WHERE proname = 'host_canonical'",
            &[],
        )
        .unwrap();
    let Some(Value::Int(oid)) = result.rows[0].get("oid") else {
        panic!("routine OID");
    };
    engine
        .durable
        .ranges
        .write()
        .get_mut("public.r")
        .unwrap()
        .canonical_oid = Some(*oid as u32);
    let control = ProductionControl::uncontrolled();
    let lower = uqa_sql::SQLParam::typed_scalar(Value::Str("1".into()), ColumnType::Text);
    let upper = uqa_sql::SQLParam::typed_scalar(Value::Int(4), ColumnType::Integer);
    let bounds = || RangeParameterBounds::Bounds {
        lower: Some(&lower),
        upper: Some(&upper),
        lower_inclusive: true,
        upper_inclusive: true,
    };
    let value = engine
        .create_range_parameter_with_control("r", bounds(), &control)
        .unwrap();
    let multi = engine
        .create_multirange_parameter_with_control("rs", std::slice::from_ref(&value), &control)
        .unwrap();
    let result = engine
        .sql(
            "SELECT $1::text AS v, currval('host_canonical_calls') AS calls",
            std::slice::from_ref(&multi),
        )
        .unwrap();
    assert_eq!(result.rows[0].get("v"), Some(&Value::Str("{[1,4]}".into())));
    assert_eq!(result.rows[0].get("calls"), Some(&Value::Int(1)));
    engine.sql("CREATE OR REPLACE FUNCTION host_canonical(r) RETURNS r LANGUAGE plpgsql AS $$ BEGIN PERFORM nextval('host_canonical_calls'); RETURN NULL; END $$; BEGIN; SAVEPOINT host_range", &[]).unwrap();
    let error = engine
        .create_range_parameter_with_control("r", bounds(), &control)
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("XX000"));
    assert_eq!(
        engine.sql("SELECT 1", &[]).unwrap_err().sqlstate(),
        Some("25P02")
    );
    let result = engine
        .sql(
            "ROLLBACK TO host_range; SELECT currval('host_canonical_calls') AS calls",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows[0].get("calls"), Some(&Value::Int(2)));
    engine.sql("COMMIT", &[]).unwrap();
}

#[test]
fn declared_range_input_adapters_invoke_selected_sql_routines_without_repeating_input() {
    let engine = Engine::new();
    engine
        .durable
        .ranges
        .write()
        .insert("public.r".into(), range());
    engine.sql("CREATE SEQUENCE canonical_calls; CREATE FUNCTION canonical(r) RETURNS r LANGUAGE plpgsql VOLATILE AS $$ BEGIN PERFORM nextval('canonical_calls'); RETURN $1; END $$", &[]).unwrap();
    let oid_result = engine
        .sql("SELECT oid FROM pg_proc WHERE proname = 'canonical'", &[])
        .unwrap();
    let Some(Value::Int(oid)) = oid_result.rows[0].get("oid") else {
        panic!("catalog routine identity");
    };
    let oid = *oid as u32;
    engine
        .durable
        .ranges
        .write()
        .get_mut("public.r")
        .unwrap()
        .canonical_oid = Some(oid);
    let result = engine.sql("SELECT '[1,2]'::r::text AS value", &[]).unwrap();
    assert_eq!(
        result.rows[0].get("value"),
        Some(&Value::Str("[1,2]".into()))
    );
    let calls = engine
        .sql("SELECT currval('canonical_calls') AS calls", &[])
        .unwrap();
    assert_eq!(calls.rows[0].get("calls"), Some(&Value::Int(1)));
    let null = engine.sql("SELECT NULL::r::text AS value", &[]).unwrap();
    assert_eq!(null.rows[0].get("value"), Some(&Value::Null));
    let invalid_null = engine.sql("SELECT NULL::integer::r", &[]).unwrap_err();
    assert_eq!(invalid_null.sqlstate(), Some("42846"));
    assert_eq!(invalid_null.to_string(), "cannot cast type integer to r");
    let result = engine
        .sql("SELECT '{[1,2],[2,3]}'::rs::text AS value", &[])
        .unwrap();
    assert_eq!(
        result.rows[0].get("value"),
        Some(&Value::Str("{[1,3]}".into()))
    );
    let calls = engine
        .sql("SELECT currval('canonical_calls') AS calls", &[])
        .unwrap();
    assert_eq!(calls.rows[0].get("calls"), Some(&Value::Int(4)));
    engine.sql("CREATE ROLE range_invoker; GRANT USAGE ON SEQUENCE canonical_calls TO range_invoker; REVOKE EXECUTE ON FUNCTION canonical(r) FROM PUBLIC; SET ROLE range_invoker", &[]).unwrap();
    engine.sql("SELECT '[1,2]'::r::text", &[]).unwrap();
    assert_eq!(
        engine
            .sql("SELECT canonical('[1,2]'::r)", &[])
            .unwrap_err()
            .sqlstate(),
        Some("42501")
    );
    engine
        .sql(
            "RESET ROLE; ALTER FUNCTION canonical(r) RENAME TO renamed_canonical",
            &[],
        )
        .unwrap();
    engine.sql("SELECT '[1,2]'::r::text", &[]).unwrap();
    engine.sql("CREATE OR REPLACE FUNCTION renamed_canonical(r) RETURNS r LANGUAGE plpgsql AS $$ BEGIN RETURN NULL; END $$", &[]).unwrap();
    let error = engine.sql("SELECT '[1,2]'::r", &[]).unwrap_err();
    assert_eq!(error.sqlstate(), Some("XX000"));
    assert_eq!(error.to_string(), format!("function {oid} returned NULL"));
}
