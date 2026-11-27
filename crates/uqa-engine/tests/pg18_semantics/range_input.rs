//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Range input diagnostics and canonical values across durable provider boundaries.

use uqa_engine::Engine;

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn multirange_constructor_inputs_match_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let engine = open(provider, &directory.path().join("multirange-input.db"));
    crate::pg18_oracle::verify(
        &engine,
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../uqa-sql/src/expr/scalar_range/tests/constructor-inputs-postgresql.json"
        )),
    );
}

pub(super) fn open(provider: usize, path: &std::path::Path) -> Engine {
    match provider {
        0 => Engine::new(),
        1 => Engine::open(path).unwrap(),
        2 => Engine::from_persistent_provider(std::sync::Arc::new(
            uqa_storage_sqlite::SQLiteKeyValueStorage::open(path).unwrap(),
        ))
        .unwrap(),
        3 => Engine::from_persistent_provider(std::sync::Arc::new(
            uqa_storage_redb::RedbStorage::open(path).unwrap(),
        ))
        .unwrap(),
        _ => unreachable!(),
    }
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn range_input_matches_postgresql_and_durable_reopen(#[case] provider: usize) {
    verify_declared_values(
        provider,
        include_str!("../../../../tests/parity/pg18/range_input_oracle.expected.json"),
        2,
    );
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite_native(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn declared_ranges_and_constructors_match_postgresql_after_reopen(#[case] provider: usize) {
    verify_declared_values(
        provider,
        include_str!("../../../../tests/parity/pg18/range_declaration.expected.json"),
        10,
    );
}

#[test]
fn range_cast_charges_multirange_construction_to_its_caller() {
    use uqa_core::{
        memory::{MemoryBudget, ProductionControl},
        CancellationToken, Value,
    };
    use uqa_sql::expr::EngineHook;

    let engine = Engine::new();
    engine
        .sql("CREATE TYPE textspan AS RANGE (subtype=text)", &[])
        .unwrap();
    let types = engine
        .sql(
            "SELECT 'textspan'::regtype::int AS r, 'textspan_multirange'::regtype::int AS m",
            &[],
        )
        .unwrap();
    let (Value::Int(range), Value::Int(multi)) = (&types.rows[0]["r"], &types.rows[0]["m"]) else {
        panic!("expected catalog type OIDs");
    };
    let range = u32::try_from(*range).unwrap();
    let multi = u32::try_from(*multi).unwrap();
    let input = format!("[{},{})", "a".repeat(32_768), "z".repeat(32_768));
    let value = engine
        .read_range_input(range, &input, &ProductionControl::uncontrolled())
        .unwrap();

    let token = CancellationToken::new();
    let budget = MemoryBudget::new(8 * 1024 * 1024);
    let control = ProductionControl::new(&budget, &token, &token);
    let direct = engine
        .construct_multirange(multi, std::slice::from_ref(&*value), &control)
        .unwrap();
    let constructor_peak = budget.peak();
    let expected = direct.into_parts().0;
    assert_eq!(budget.used(), 0);

    let argument_bytes = control.copy_value(&value).unwrap().reserved_bytes();
    let cast_budget = MemoryBudget::new(8 * 1024 * 1024);
    let control = ProductionControl::new(&cast_budget, &token, &token);
    let result = engine
        .cast_range_to_multirange(range, multi, &value, &control)
        .unwrap()
        .unwrap();
    assert_eq!(*result, expected);
    drop(result);
    assert_eq!(cast_budget.used(), 0);
    let required = argument_bytes + constructor_peak;
    assert!(
        cast_budget.peak() >= required,
        "cast peak {} must cover its argument ({argument_bytes}) and constructor ({constructor_peak})",
        cast_budget.peak()
    );

    let limited = MemoryBudget::new(required - 1);
    let control = ProductionControl::new(&limited, &token, &token);
    let error = engine
        .cast_range_to_multirange(range, multi, &value, &control)
        .expect_err("the cast must admit the same constructor temporaries as a direct call");
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(limited.used(), 0);
}

fn verify_declared_values(provider: usize, transcript: &str, reopen_cases: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("range-input.db");
    let engine = open(provider, &path);
    let reference: serde_json::Value = serde_json::from_str(transcript).unwrap();
    let mut initial = reference.clone();
    initial["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|case| case["reopen"] != true);
    crate::pg18_oracle::verify(&engine, &initial.to_string());
    let engine = if provider == 0 {
        engine
    } else {
        drop(engine);
        open(provider, &path)
    };
    let mut reopened = reference;
    reopened["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|case| case["reopen"] == true);
    assert_eq!(reopened["cases"].as_array().unwrap().len(), reopen_cases);
    crate::pg18_oracle::verify(&engine, &reopened.to_string());
}

#[rstest::rstest]
#[case::sqlite_native(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn declared_range_publication_follows_transaction_visibility(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let engine = open(provider, &directory.path().join("range-peers.db"));
    crate::pg18_peer::verify(
        &engine,
        include_str!("../../../../tests/parity/pg18/range_declaration_sessions.expected.json"),
    );
}
