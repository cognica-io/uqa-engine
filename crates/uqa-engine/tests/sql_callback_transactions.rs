//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::sync::{Arc, Weak};

use tempfile::tempdir;
use uqa_core::Value;
use uqa_engine::{Engine, SQLAggregateState, SQLTableFunctionResult};
use uqa_sql::SQLError;

const SCALAR_KEY: &str = "callback.scalar";
const TABLE_KEY: &str = "callback.table";
const AGGREGATE_KEY: &str = "callback.aggregate";
const NESTED_VIEW_KEY: &str = "callback.nested_view";

fn mutate_scoring_params(engine: &Weak<Engine>, key: &str) -> Result<(), SQLError> {
    let engine = engine
        .upgrade()
        .ok_or_else(|| SQLError::Internal("callback engine was dropped".into()))?;
    engine.save_scoring_params(key, r#"{"alpha":1.0}"#)
}

struct MutatingAggregate {
    engine: Weak<Engine>,
}

impl SQLAggregateState for MutatingAggregate {
    fn observe(&mut self, _args: &[Value]) -> Result<(), SQLError> {
        mutate_scoring_params(&self.engine, AGGREGATE_KEY)?;
        Err(SQLError::Internal(
            "registered aggregate failed after catalog mutation".into(),
        ))
    }

    fn finish(&self) -> Result<Value, SQLError> {
        Ok(Value::Null)
    }
}

fn assert_scoring_params_absent(engine: &Engine, key: &str) {
    assert!(
        engine.load_scoring_params(key).unwrap().is_none(),
        "failed callback left scoring params `{key}` visible"
    );
}

fn assert_nested_view_error(error: &SQLError) {
    assert!(
        error
            .to_string()
            .contains("nested view callback failed after mutations"),
        "{error}"
    );
}

fn assert_nested_view_failure_rolls_back(engine: &Arc<Engine>) -> i64 {
    engine
        .sql("CREATE SEQUENCE nested_view_sequence START 1", &[])
        .unwrap();
    let sequence_before = engine
        .sequence_state("nested_view_sequence")
        .unwrap()
        .unwrap()
        .1
        .current;

    let callback_engine = Arc::downgrade(engine);
    engine
        .register_scalar_function("fail_nested_view", move |_args: &[Value]| {
            mutate_scoring_params(&callback_engine, NESTED_VIEW_KEY)?;
            Err(SQLError::Internal(
                "nested view callback failed after mutations".into(),
            ))
        })
        .unwrap();
    engine
        .sql(
            "CREATE VIEW nested_sequence_inner AS \
             SELECT nextval('nested_view_sequence') AS marker",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE VIEW nested_callback_outer AS \
             SELECT marker, fail_nested_view(marker) AS failure \
             FROM nested_sequence_inner",
            &[],
        )
        .unwrap();

    let error = engine
        .sql("SELECT marker, failure FROM nested_callback_outer", &[])
        .unwrap_err();
    assert_nested_view_error(&error);
    assert_scoring_params_absent(engine, NESTED_VIEW_KEY);
    assert_eq!(
        engine
            .sequence_state("nested_view_sequence")
            .unwrap()
            .unwrap()
            .1
            .current,
        sequence_before,
        "failed nested view execution advanced its sequence"
    );
    assert!(
        engine.currval("nested_view_sequence").is_err(),
        "failed nextval became the session currval"
    );

    let committed_value = engine.nextval("nested_view_sequence").unwrap();
    assert_eq!(
        engine.currval("nested_view_sequence").unwrap(),
        committed_value
    );
    engine
        .sql(
            "CREATE VIEW nested_setval_inner AS \
             SELECT setval('nested_view_sequence', 99) AS marker",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE VIEW nested_setval_outer AS \
             SELECT marker, fail_nested_view(marker) AS failure \
             FROM nested_setval_inner",
            &[],
        )
        .unwrap();

    let error = engine
        .sql("SELECT marker, failure FROM nested_setval_outer", &[])
        .unwrap_err();
    assert_nested_view_error(&error);
    assert_scoring_params_absent(engine, NESTED_VIEW_KEY);
    assert_eq!(
        engine.currval("nested_view_sequence").unwrap(),
        committed_value,
        "failed setval replaced the session currval"
    );
    assert_eq!(
        engine
            .sequence_state("nested_view_sequence")
            .unwrap()
            .unwrap()
            .1
            .current,
        committed_value,
        "failed setval changed the sequence state"
    );
    committed_value
}

#[test]
fn failed_registered_callbacks_roll_back_captured_persistent_mutations() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("callback-transactions.sqlite");
    let engine = Arc::new(Engine::open(&database).unwrap());
    engine
        .sql("CREATE TABLE callback_inputs (value INTEGER)", &[])
        .unwrap();
    engine
        .sql("INSERT INTO callback_inputs (value) VALUES (1)", &[])
        .unwrap();

    let scalar_engine = Arc::downgrade(&engine);
    engine
        .register_scalar_function("mutating_scalar", move |_args: &[Value]| {
            mutate_scoring_params(&scalar_engine, SCALAR_KEY)?;
            Err(SQLError::Internal(
                "registered scalar failed after catalog mutation".into(),
            ))
        })
        .unwrap();

    let table_engine = Arc::downgrade(&engine);
    engine
        .register_table_function(
            "mutating_table",
            move |_args: &[Value]| -> Result<SQLTableFunctionResult, SQLError> {
                mutate_scoring_params(&table_engine, TABLE_KEY)?;
                Err(SQLError::Internal(
                    "registered table function failed after catalog mutation".into(),
                ))
            },
        )
        .unwrap();

    let aggregate_engine = Arc::downgrade(&engine);
    engine
        .register_aggregate_function("mutating_aggregate", move || MutatingAggregate {
            engine: aggregate_engine.clone(),
        })
        .unwrap();

    let scalar_error = engine.sql("SELECT mutating_scalar()", &[]).unwrap_err();
    assert!(scalar_error
        .to_string()
        .contains("registered scalar failed"));
    assert_scoring_params_absent(&engine, SCALAR_KEY);

    let table_error = engine
        .sql("SELECT * FROM mutating_table()", &[])
        .unwrap_err();
    assert!(table_error
        .to_string()
        .contains("registered table function failed"));
    assert_scoring_params_absent(&engine, TABLE_KEY);

    let aggregate_error = engine
        .sql("SELECT mutating_aggregate(value) FROM callback_inputs", &[])
        .unwrap_err();
    assert!(aggregate_error
        .to_string()
        .contains("registered aggregate failed"));
    assert_scoring_params_absent(&engine, AGGREGATE_KEY);

    let reopened = Engine::open(&database).unwrap();
    for key in [SCALAR_KEY, TABLE_KEY, AGGREGATE_KEY] {
        assert_scoring_params_absent(&reopened, key);
    }
}

#[test]
fn nested_view_mutations_are_writer_classified_and_rolled_back() {
    let memory = Arc::new(Engine::new());
    assert_nested_view_failure_rolls_back(&memory);

    let directory = tempdir().unwrap();
    let database = directory.path().join("nested-view-transactions.sqlite");
    let persistent = Arc::new(Engine::open(&database).unwrap());
    let sequence_before = assert_nested_view_failure_rolls_back(&persistent);

    drop(persistent);
    let reopened = Engine::open(&database).unwrap();
    assert_scoring_params_absent(&reopened, NESTED_VIEW_KEY);
    assert!(
        reopened.currval("nested_view_sequence").is_err(),
        "a reopened logical session inherited another session's currval"
    );
    assert_eq!(
        reopened
            .sequence_state("nested_view_sequence")
            .unwrap()
            .unwrap()
            .1
            .current,
        sequence_before,
        "failed nested view execution persisted a sequence advance"
    );
}

fn assert_explicit_sequence_currval_rollback(engine: &Engine) -> i64 {
    engine
        .sql("CREATE SEQUENCE explicit_rollback_sequence START 10", &[])
        .unwrap();
    let initial = engine
        .sequence_state("explicit_rollback_sequence")
        .unwrap()
        .unwrap()
        .1
        .current;

    engine.begin().unwrap();
    assert_eq!(engine.nextval("explicit_rollback_sequence").unwrap(), 10);
    engine.rollback().unwrap();
    assert!(engine.currval("explicit_rollback_sequence").is_err());
    assert_eq!(
        engine
            .sequence_state("explicit_rollback_sequence")
            .unwrap()
            .unwrap()
            .1
            .current,
        initial
    );

    let committed = engine.nextval("explicit_rollback_sequence").unwrap();
    engine.begin().unwrap();
    assert_eq!(engine.setval("explicit_rollback_sequence", 99).unwrap(), 99);
    engine.rollback().unwrap();
    assert_eq!(
        engine.currval("explicit_rollback_sequence").unwrap(),
        committed
    );
    assert_eq!(
        engine
            .sequence_state("explicit_rollback_sequence")
            .unwrap()
            .unwrap()
            .1
            .current,
        committed
    );

    engine.begin().unwrap();
    engine.savepoint("sequence_point").unwrap();
    assert_eq!(
        engine.nextval("explicit_rollback_sequence").unwrap(),
        committed + 1
    );
    engine.rollback_to_savepoint("sequence_point").unwrap();
    assert_eq!(
        engine.currval("explicit_rollback_sequence").unwrap(),
        committed
    );
    engine.rollback().unwrap();
    assert_eq!(
        engine.currval("explicit_rollback_sequence").unwrap(),
        committed
    );
    committed
}

#[test]
fn sequence_currval_is_restored_by_transactions_and_savepoints() {
    let memory = Engine::new();
    assert_explicit_sequence_currval_rollback(&memory);

    let directory = tempdir().unwrap();
    let database = directory.path().join("sequence-currval-rollback.sqlite");
    let persistent = Engine::open(&database).unwrap();
    let committed = assert_explicit_sequence_currval_rollback(&persistent);
    drop(persistent);

    let reopened = Engine::open(&database).unwrap();
    assert!(reopened.currval("explicit_rollback_sequence").is_err());
    assert_eq!(
        reopened
            .sequence_state("explicit_rollback_sequence")
            .unwrap()
            .unwrap()
            .1
            .current,
        committed
    );
}
