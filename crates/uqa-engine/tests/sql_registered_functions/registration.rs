//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::Barrier;
use uqa_engine::{SQLAggregateFactory, SQLFunctionContext, SQLFunctionRegistrationError};

fn options() -> SQLFunctionOptions {
    SQLFunctionOptions::read_only(SQLFunctionVolatility::Volatile)
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite_native(1)]
#[case::sqlite_kv(2)]
#[case::redb(3)]
fn streaming_host_arguments_observe_current_enum_labels(#[case] provider: usize) {
    struct Echo;
    impl SQLTableFunction for Echo {
        fn call_stream(&self, args: &[Value]) -> Result<SQLTableFunctionStream, SQLError> {
            let [Value::Str(label)] = args else {
                panic!("host argument must be a label: {args:?}")
            };
            Ok(SQLTableFunctionStream::new(
                ["label"],
                std::iter::once(Ok(vec![Value::Str(label.clone())])),
            ))
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let engine = fallible::open(provider, &directory.path().join("labels.db"));
    engine
        .register_table_function_with_options("host_label", options(), Echo)
        .unwrap();
    engine.sql("CREATE TYPE host_mood AS ENUM ('ready'); CREATE TABLE host_values (m host_mood); INSERT INTO host_values VALUES ('ready')", &[]).unwrap();
    for expected in ["ready", "renamed"] {
        for query in [
            "SELECT label FROM host_values, LATERAL host_label(m)",
            "SELECT host_label(m) AS label FROM host_values",
        ] {
            let result = engine.sql(query, &[]).unwrap();
            assert_eq!(
                result.rows[0]["label"],
                Value::Str(expected.into()),
                "{query}: {:?}",
                result.rows
            );
        }
        if expected == "ready" {
            engine
                .sql(
                    "ALTER TYPE host_mood RENAME VALUE 'ready' TO 'renamed'",
                    &[],
                )
                .unwrap();
        }
    }
}

struct ContextFunction;

fn context_value(context: &SQLFunctionContext) -> Value {
    context
        .get::<i64>()
        .copied()
        .map_or(Value::Null, Value::Int)
}

impl SQLScalarFunction for ContextFunction {
    fn call_with_context(
        &self,
        context: &SQLFunctionContext,
        _: &[Value],
    ) -> Result<Value, SQLError> {
        Ok(context_value(context))
    }
}

impl SQLTableFunction for ContextFunction {
    fn call_with_context(
        &self,
        context: &SQLFunctionContext,
        _: &[Value],
    ) -> Result<SQLTableFunctionResult, SQLError> {
        Ok(SQLTableFunctionResult::new(
            ["v"],
            vec![vec![context_value(context)]],
        ))
    }

    fn call_stream_with_context(
        &self,
        context: &SQLFunctionContext,
        args: &[Value],
    ) -> Result<SQLTableFunctionStream, SQLError> {
        SQLTableFunction::call_with_context(self, context, args).map(Into::into)
    }
}

struct ContextState(Value);

impl SQLAggregateState for ContextState {
    fn observe(&mut self, _: &[Value]) -> Result<(), SQLError> {
        Ok(())
    }
    fn finish(&self) -> Result<Value, SQLError> {
        Ok(self.0.clone())
    }
}

impl SQLAggregateFactory for ContextFunction {
    fn try_create_state_with_context(
        &self,
        context: &SQLFunctionContext,
    ) -> Result<Box<dyn SQLAggregateState>, SQLError> {
        Ok(Box::new(ContextState(context_value(context))))
    }
}

fn assert_context(engine: &Engine, expected: &Value) {
    for query in [
        "SELECT host_context() AS v",
        "SELECT v FROM host_rows()",
        "SELECT host_group(n) AS v FROM (VALUES (1), (2)) s(n)",
        "SELECT host_group(n) OVER () AS v FROM (VALUES (1), (2)) s(n)",
        "WITH s AS MATERIALIZED (SELECT * FROM host_rows()) SELECT v FROM s",
        "SELECT host_context() AS v FROM (VALUES (1), (2)) s(n) WHERE host_context() IS NOT NULL OR n > 0",
    ] {
        let rows = engine.sql(query, &[]).unwrap().rows;
        assert!(!rows.is_empty(), "{query}");
        for row in rows { assert_eq!(&row["v"], expected, "{query}"); }
    }
    engine
        .sql(
            "BEGIN; DECLARE context_rows CURSOR FOR SELECT v FROM host_rows()",
            &[],
        )
        .unwrap();
    let rows = engine.sql("FETCH ALL FROM context_rows", &[]).unwrap().rows;
    assert_eq!(&rows[0]["v"], expected);
    engine.sql("CLOSE context_rows; COMMIT", &[]).unwrap();
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite_native(1)]
#[case::sqlite_kv(2)]
#[case::redb(3)]
fn context_belongs_to_the_invoking_session_and_survives_portal_execution(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let root = fallible::open(provider, &directory.path().join("context.db"));
    root.install_sql_function_context(SQLFunctionContext::new(17_i64))
        .unwrap();
    assert!(matches!(
        root.install_sql_function_context(SQLFunctionContext::new(999_i64)),
        Err(SQLFunctionRegistrationError::Invalid(_))
    ));
    let mut scalar = root
        .register_scalar_function_retained("host_context", options(), ContextFunction)
        .unwrap();
    let mut table = root
        .register_table_function_retained("host_rows", options(), ContextFunction)
        .unwrap();
    let mut aggregate = root
        .register_aggregate_function_retained("host_group", options(), ContextFunction)
        .unwrap();
    assert_context(&root, &Value::Int(17));
    if provider != 0 {
        let sibling = root.new_session().unwrap();
        assert_context(&sibling, &Value::Null);
        sibling
            .install_sql_function_context(SQLFunctionContext::new(29_i64))
            .unwrap();
        assert_context(&sibling, &Value::Int(29));
        root.close().unwrap();
        drop(root);
        assert_context(&sibling, &Value::Int(29));
        sibling.close().unwrap();
        drop(sibling);
    } else {
        root.close().unwrap();
        drop(root);
    }
    scalar.unregister().unwrap();
    table.unregister().unwrap();
    aggregate.unregister().unwrap();
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite_native(1)]
#[case::sqlite_kv(2)]
#[case::redb(3)]
fn registry_changes_rebind_all_sessions_and_old_handles_preserve_replacements(
    #[case] provider: usize,
) {
    let directory = tempfile::tempdir().unwrap();
    let root = fallible::open(provider, &directory.path().join("plans.db"));
    let mut scalar = root
        .register_scalar_function_retained("calc", options(), |args: &[Value]| Ok(args[0].clone()))
        .unwrap();
    let sibling = (provider != 0).then(|| root.new_session().unwrap());
    let reader = sibling.as_ref().unwrap_or(&root);
    let query = "SELECT calc(n) AS v FROM (VALUES (1), (2)) s(n)";
    let prepared = reader.prepare_statement(query, &[]).unwrap();
    assert_eq!(
        reader
            .execute_prepared_statement(&prepared, &[])
            .unwrap()
            .rows
            .len(),
        2
    );
    assert_eq!(reader.sql(query, &[]).unwrap().rows.len(), 2);
    reader.sql("BEGIN; SAVEPOINT before_change", &[]).unwrap();
    let mut aggregate = root
        .register_aggregate_function_retained("calc", options(), SumSquares::default)
        .unwrap();
    for result in [
        reader.sql(query, &[]).unwrap(),
        reader.execute_prepared_statement(&prepared, &[]).unwrap(),
    ] {
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0]["v"], Value::Int(5));
    }
    reader.sql("ROLLBACK TO before_change", &[]).unwrap();
    assert_eq!(reader.sql(query, &[]).unwrap().rows.len(), 1);
    reader.sql("ROLLBACK", &[]).unwrap();
    assert_eq!(reader.sql(query, &[]).unwrap().rows.len(), 1);
    aggregate.unregister().unwrap();
    assert_eq!(reader.sql(query, &[]).unwrap().rows.len(), 2);
    assert_eq!(
        reader
            .execute_prepared_statement(&prepared, &[])
            .unwrap()
            .rows
            .len(),
        2
    );
    let mut replacement = root
        .register_scalar_function_retained("CALC", options(), |_: &[Value]| Ok(Value::Int(41)))
        .unwrap();
    scalar.unregister().unwrap();
    scalar.unregister().unwrap();
    assert_eq!(
        reader.sql("SELECT calc(1) AS v", &[]).unwrap().rows[0]["v"],
        Value::Int(41)
    );
    let invalid = root.register_scalar_function_retained(
        "calc",
        SQLFunctionOptions::new(SQLFunctionVolatility::Stable, true),
        |_: &[Value]| Ok(Value::Int(99)),
    );
    assert!(matches!(
        invalid,
        Err(SQLFunctionRegistrationError::Invalid(_))
    ));
    assert_eq!(
        reader.sql("SELECT calc(1) AS v", &[]).unwrap().rows[0]["v"],
        Value::Int(41)
    );
    replacement.unregister().unwrap();
    assert_eq!(
        reader
            .execute_prepared_statement(&prepared, &[])
            .unwrap_err()
            .sqlstate(),
        Some("42883")
    );
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite_native(1)]
#[case::sqlite_kv(2)]
#[case::redb(3)]
fn active_execution_rejects_registration_and_retirement_without_consuming_the_handle(
    #[case] provider: usize,
) {
    let directory = tempfile::tempdir().unwrap();
    let root = Arc::new(fallible::open(provider, &directory.path().join("busy.db")));
    let entered = Arc::new(Barrier::new(2));
    let released = Arc::new(Barrier::new(2));
    let nested_busy = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut registration = {
        let entered = Arc::clone(&entered);
        let released = Arc::clone(&released);
        let nested_busy = Arc::clone(&nested_busy);
        let caller = Arc::downgrade(&root);
        root.register_scalar_function_retained("blocking_host", options(), move |_: &[Value]| {
            let caller = caller.upgrade().unwrap();
            nested_busy.store(
                matches!(
                    caller.register_scalar_function_retained(
                        "nested",
                        options(),
                        |_: &[Value]| Ok(Value::Null)
                    ),
                    Err(SQLFunctionRegistrationError::Busy)
                ),
                Ordering::SeqCst,
            );
            entered.wait();
            released.wait();
            Ok(Value::Int(23))
        })
        .unwrap()
    };
    let worker = Arc::clone(&root);
    let work = std::thread::spawn(move || worker.sql("SELECT blocking_host() AS v", &[]));
    entered.wait();
    let retirement = registration.unregister();
    let replacement =
        root.register_scalar_function_retained("blocking_host", options(), |_: &[Value]| {
            Ok(Value::Int(99))
        });
    released.wait();
    assert_eq!(work.join().unwrap().unwrap().rows[0]["v"], Value::Int(23));
    assert!(matches!(
        retirement,
        Err(SQLFunctionRegistrationError::Busy)
    ));
    assert!(matches!(
        replacement,
        Err(SQLFunctionRegistrationError::Busy)
    ));
    registration.unregister().unwrap();
    assert_eq!(
        root.sql("SELECT blocking_host()", &[])
            .unwrap_err()
            .sqlstate(),
        Some("42883")
    );
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite_native(1)]
#[case::sqlite_kv(2)]
#[case::redb(3)]
fn retained_factories_and_states_keep_registration_identity_alive(#[case] provider: usize) {
    use uqa_engine::functions::AggregateFunctionRegistry;
    let directory = tempfile::tempdir().unwrap();
    let engine = fallible::open(provider, &directory.path().join("retained.db"));
    let mut registration = engine
        .register_aggregate_function_retained("retained_sum", options(), SumSquares::default)
        .unwrap();
    let factory = engine
        .registered_aggregate_function("retained_sum")
        .unwrap();
    let mut state = factory.try_create_state().unwrap();
    assert!(matches!(
        registration.unregister(),
        Err(SQLFunctionRegistrationError::Busy)
    ));
    drop(factory);
    assert!(matches!(
        registration.unregister(),
        Err(SQLFunctionRegistrationError::Busy)
    ));
    state.observe(&[Value::Int(3)]).unwrap();
    assert_eq!(state.finish().unwrap(), Value::Int(9));
    state.close().unwrap();
    state.close().unwrap();
    assert!(state.observe(&[Value::Int(4)]).is_err());
    assert!(state.finish().is_err());
    drop(state);
    registration.unregister().unwrap();
}
