//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_execution::query::relational::QueryExpressionFactory;
use uqa_execution::RowSchemaExecution;

#[rstest::rstest]
#[case::memory_binding(Provider::Memory, false)]
#[case::memory_borrowed(Provider::Memory, true)]
#[case::sqlite_native_binding(Provider::SQLiteNative, false)]
#[case::sqlite_native_borrowed(Provider::SQLiteNative, true)]
#[case::sqlite_kv_binding(Provider::SQLiteKeyValue, false)]
#[case::sqlite_kv_borrowed(Provider::SQLiteKeyValue, true)]
#[case::redb_binding(Provider::Redb, false)]
#[case::redb_borrowed(Provider::Redb, true)]
fn native_expression_scope_admission_and_borrowed_evaluation(
    #[case] provider: Provider,
    #[case] borrowed: bool,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("expression-scope.db"));
    let UnifiedPlan::Query(mut query) = UnifiedPlan::lower(
        uqa_sql::compile("WITH unused AS (SELECT 1) SELECT 42")
            .unwrap()
            .remove(0),
    ) else {
        panic!("query")
    };
    query.ctes[0].name = "x".repeat(4 << 20);
    let mut scope = crate::capabilities::query_scope::new_for_statement(&engine, None).unwrap();
    scope.insert_deferred(query.ctes.remove(0).into());
    let resources = engine.query_retention_control().unwrap();
    let memory = resources.memory();
    let before = memory.used();
    let hook = borrowed.then(|| engine.bind_scope(&scope).unwrap());
    let retained = memory.used();
    if borrowed {
        assert!(retained >= before + (4 << 20));
    }
    let occupied = memory.reserve(memory.available() - (64 << 10)).unwrap();
    let mut outcome = None;
    let allocation = allocation_counter::measure(|| {
        outcome = Some(if let Some(hook) = &hook {
            hook.expression_evaluator(&[]).and_then(|evaluator| {
                assert!(
                    memory.used() > retained + occupied.bytes(),
                    "evaluator header must retain its allowance"
                );
                let row = uqa_execution::RowSchema::default();
                let value = evaluator
                    .evaluate(
                        &uqa_sql::ScalarExpr::Literal(Value::Int(42)),
                        &row.view(&uqa_execution::PhysicalRow::default()),
                    )
                    .map_err(uqa_execution::physical::physical_exec_error)?;
                assert_eq!(value, Value::Int(42));
                Ok(())
            })
        } else {
            engine.bind_scope(&scope).map(drop)
        });
    });
    assert!(
        allocation.bytes_total < 256 << 10,
        "{provider:?} borrowed={borrowed}: {allocation:?}"
    );
    if borrowed {
        outcome.unwrap().unwrap();
    } else {
        assert_eq!(outcome.unwrap().unwrap_err().sqlstate(), Some("53200"));
    }
    assert_eq!(memory.used(), retained + occupied.bytes());
    drop(occupied);
    if !borrowed {
        let hook = engine.bind_scope(&scope).unwrap();
        assert!(memory.used() >= before + (4 << 20));
        drop(hook);
        assert_eq!(memory.used(), before);
    }
    let runtime = engine.query_runtime_view();
    for token in [resources.cancellation(), runtime.cancellation] {
        token.cancel();
        let result = match &hook {
            Some(hook) => hook.expression_evaluator(&[]).map(drop),
            None => engine.bind_scope(&scope).map(drop),
        };
        assert_eq!(result.unwrap_err().sqlstate(), Some("57014"));
        assert_eq!(memory.used(), retained);
        token.reset();
    }
    drop((hook, scope));
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
}
