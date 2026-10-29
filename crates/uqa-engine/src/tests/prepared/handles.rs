//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::tests::providers::Provider;
use crate::Engine;
use uqa_core::Value;
use uqa_sql::{ColumnType, SQLParam};

#[test]
fn host_handles_keep_native_descriptors_and_custom_generic_selection() {
    let engine = Engine::new();
    let handle = engine
        .prepare_statement("SELECT $1::integer AS x, $1 + 1 AS x", &[])
        .unwrap();
    let description = engine.describe_prepared_statement(&handle).unwrap();
    assert_eq!(description.parameter_types, vec![Some(ColumnType::Integer)]);
    assert_eq!(description.result_schema.unwrap().columns(), &["x", "x"]);
    for mode in ["force_custom_plan", "force_generic_plan"] {
        engine
            .sql(&format!("SET plan_cache_mode = {mode}"), &[])
            .unwrap();
        let result = engine
            .execute_prepared_statement(&handle, &[SQLParam::Scalar(Value::Int(7))])
            .unwrap();
        assert_eq!(result.columns, ["x", "x"]);
        assert_eq!(result.value_at(0, 0), Some(&Value::Int(7)));
        assert_eq!(result.value_at(0, 1), Some(&Value::Int(8)));
    }
    let entry = engine
        .session
        .prepared
        .read()
        .values()
        .next()
        .unwrap()
        .clone();
    assert_eq!(
        entry.source_sql.as_ref().map(|sql| sql.as_str()),
        Some("SELECT $1::integer AS x, $1 + 1 AS x")
    );
    assert!(!entry.from_sql);
    assert_eq!(entry.custom_plans, 1);
    assert_eq!(entry.generic_plans, 1);
    engine.sql("DISCARD PLANS", &[]).unwrap();
    engine
        .execute_prepared_statement(&handle, &[SQLParam::Scalar(Value::Int(9))])
        .unwrap();
    engine.finalize_prepared_statement(&handle);
    assert!(engine.session.prepared.read().is_empty());
}

#[test]
fn host_handle_cannot_rebind_a_recreated_name_or_another_session() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("identities.db")).unwrap();
    let handle = engine.prepare_statement("SELECT 7 AS value", &[]).unwrap();
    let name = engine
        .session
        .prepared
        .read()
        .keys()
        .next()
        .unwrap()
        .to_string();
    let sibling = engine.new_session().unwrap();
    assert_eq!(
        sibling
            .validate_prepared_statement(&handle)
            .unwrap_err()
            .sqlstate(),
        Some("26000")
    );
    sibling.finalize_prepared_statement(&handle);
    engine.validate_prepared_statement(&handle).unwrap();
    engine.deallocate_prepared(Some(&name));
    engine
        .register_prepared(
            name.clone(),
            uqa_sql::compile("SELECT 8 AS value").unwrap().remove(0),
        )
        .unwrap();
    assert_eq!(
        engine
            .validate_prepared_statement(&handle)
            .unwrap_err()
            .sqlstate(),
        Some("26000")
    );
    assert_eq!(
        engine
            .execute_prepared_statement(&handle, &[])
            .unwrap_err()
            .sqlstate(),
        Some("26000")
    );
    engine.finalize_prepared_statement(&handle);
    assert!(engine.session.prepared.read().contains_key(&name));
    let current = engine.prepare_statement("SELECT 9", &[]).unwrap();
    engine.sql("DISCARD ALL", &[]).unwrap();
    assert_eq!(
        engine
            .validate_prepared_statement(&current)
            .unwrap_err()
            .sqlstate(),
        Some("26000")
    );
    engine.finalize_prepared_statement(&current);
}

#[test]
fn host_preparation_preserves_declared_types_and_failed_registration_is_not_published() {
    let engine = Engine::new();
    engine
        .sql("CREATE DOMAIN positive AS integer CHECK (VALUE > 0)", &[])
        .unwrap();
    let handle = engine
        .prepare_statement("SELECT $1 AS n", &[ColumnType::Named("positive".into())])
        .unwrap();
    let description = engine.describe_prepared_statement(&handle).unwrap();
    assert!(matches!(
        description.parameter_types[0],
        Some(ColumnType::Domain { .. })
    ));
    engine
        .execute_prepared_statement(&handle, &[SQLParam::Scalar(Value::Int(3))])
        .unwrap();
    let error = engine
        .execute_prepared_statement(&handle, &[SQLParam::Scalar(Value::Int(-1))])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("23514"));
    assert!(engine
        .prepare_statement("SELECT missing FROM no_table", &[])
        .is_err());
    assert!(engine.prepare_statement("SELECT 1; SELECT 2", &[]).is_err());
    assert_eq!(engine.session.prepared.read().len(), 1);
    engine.finalize_prepared_statement(&handle);
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn host_execution_keeps_transaction_boundaries_and_fixed_result_schemas(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("prepared.db"));
    engine.sql("CREATE TABLE items (n integer)", &[]).unwrap();
    let insert = engine
        .prepare_statement("INSERT INTO items VALUES ($1) RETURNING n", &[])
        .unwrap();
    let select = engine
        .prepare_statement("SELECT * FROM items", &[])
        .unwrap();
    engine.sql("BEGIN", &[]).unwrap();
    engine
        .execute_prepared_statement(&insert, &[SQLParam::Scalar(Value::Int(1))])
        .unwrap();
    assert_eq!(engine.transaction_depth(), 1);
    engine.sql("ROLLBACK", &[]).unwrap();
    assert!(engine
        .execute_prepared_statement(&select, &[])
        .unwrap()
        .rows
        .is_empty());
    engine
        .execute_prepared_statement(&insert, &[SQLParam::Scalar(Value::Int(2))])
        .unwrap();
    assert_eq!(engine.transaction_depth(), 0);
    assert_eq!(
        engine
            .execute_prepared_statement(&select, &[])
            .unwrap()
            .rows
            .len(),
        1
    );
    engine
        .sql("ALTER TABLE items ADD COLUMN extra text", &[])
        .unwrap();
    assert_eq!(
        engine
            .execute_prepared_statement(&select, &[])
            .unwrap_err()
            .sqlstate(),
        Some("0A000")
    );
    engine.finalize_prepared_statement(&insert);
    engine.finalize_prepared_statement(&select);
}

#[test]
fn prepared_transaction_commands_are_scheduled_as_transaction_commands() {
    let engine = Engine::new();
    let begin = engine.prepare_statement("BEGIN", &[]).unwrap();
    let rollback = engine.prepare_statement("ROLLBACK", &[]).unwrap();
    engine.execute_prepared_statement(&begin, &[]).unwrap();
    assert_eq!(engine.transaction_depth(), 1);
    engine
        .sql("CREATE TABLE rolled_back (n integer)", &[])
        .unwrap();
    engine.execute_prepared_statement(&rollback, &[]).unwrap();
    assert_eq!(engine.transaction_depth(), 0);
    assert!(engine.sql("SELECT * FROM rolled_back", &[]).is_err());
    engine.finalize_prepared_statement(&begin);
    engine.finalize_prepared_statement(&rollback);
}
