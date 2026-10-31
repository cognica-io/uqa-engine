//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_execution::{query::catalog_expression, RowSchema};
use uqa_sql::{ast::Expr, plan::ExpressionPlan, ResultRow, SQLError};

#[rstest::rstest]
#[case::memory_syntax(Provider::Memory, "syntax")]
#[case::memory_analyzed(Provider::Memory, "analyzed")]
#[case::memory_scalar(Provider::Memory, "scalar")]
#[case::sqlite_native_syntax(Provider::SQLiteNative, "syntax")]
#[case::sqlite_native_analyzed(Provider::SQLiteNative, "analyzed")]
#[case::sqlite_native_scalar(Provider::SQLiteNative, "scalar")]
#[case::sqlite_kv_syntax(Provider::SQLiteKeyValue, "syntax")]
#[case::sqlite_kv_analyzed(Provider::SQLiteKeyValue, "analyzed")]
#[case::sqlite_kv_scalar(Provider::SQLiteKeyValue, "scalar")]
#[case::redb_syntax(Provider::Redb, "syntax")]
#[case::redb_analyzed(Provider::Redb, "analyzed")]
#[case::redb_scalar(Provider::Redb, "scalar")]
fn native_catalog_expressions_admit_before_copying_and_recover(
    #[case] provider: Provider,
    #[case] input: &str,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("catalog-expression.db"));
    engine.sql("SELECT 1", &[]).unwrap();
    let syntax = Expr::Literal(Value::Str("x".repeat(4 << 20)));
    let analyzed = ExpressionPlan::lower(syntax.clone());
    let schema = RowSchema::default();
    let row = ResultRow::new();
    let evaluate = || -> Result<Value, SQLError> {
        let scope = crate::capabilities::query_scope::new_for_current_routine(&engine)?;
        match input {
            "syntax" => {
                catalog_expression::eval_lowered_expression(&engine, scope, &syntax, None, &[])
            }
            "analyzed" => catalog_expression::eval_expression_plan_with_schema(
                &engine,
                scope,
                &analyzed,
                &row,
                &schema,
                &[],
            ),
            "scalar" => catalog_expression::eval_scalar_plan_with_schema(
                &engine,
                scope,
                &analyzed.scalar,
                &row,
                &schema,
                &[],
            ),
            _ => unreachable!("expression input"),
        }
    };
    let resources = engine.query_retention_control().unwrap();
    let runtime = engine.query_runtime_view();
    let memory = resources.memory();
    let before = memory.used();
    let occupied = memory.reserve(memory.available() - (64 << 10)).unwrap();
    let mut result = None;
    let allocation = allocation_counter::measure(|| result = Some(evaluate()));
    assert_eq!(result.unwrap().unwrap_err().sqlstate(), Some("53200"));
    assert!(
        allocation.bytes_total < 256 << 10,
        "{provider:?} {input}: {allocation:?}"
    );
    assert_eq!(memory.used(), before + occupied.bytes());
    drop(occupied);
    for token in [resources.cancellation(), runtime.cancellation] {
        token.cancel();
        assert_eq!(evaluate().unwrap_err().sqlstate(), Some("57014"));
        assert_eq!(memory.used(), before);
        token.reset();
    }
    let Value::Str(value) = evaluate().unwrap() else {
        panic!("catalog literal value")
    };
    assert_eq!(value.len(), 4 << 20);
    assert!(value.bytes().all(|byte| byte == b'x'));
    drop(value);
    assert_eq!(memory.used(), before);
    engine.sql("SELECT 42", &[]).unwrap();
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
}
