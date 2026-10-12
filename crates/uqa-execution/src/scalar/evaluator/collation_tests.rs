//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL's independent collation-value reference through ordinary and admitted physical evaluation.

use super::*;
use std::sync::atomic::{AtomicI64, Ordering};
use uqa_core::{memory::MemoryBudget, CancellationToken};
use uqa_sql::{ast::FunctionDispatch, expr::EngineHook, ColumnType, RowSchema};

#[derive(Default)]
struct Sequence(AtomicI64);

impl EngineHook for Sequence {
    fn nextval(&self, _: &str) -> Result<i64, SQLError> {
        Ok(self.0.fetch_add(1, Ordering::Relaxed) + 1)
    }
    fn currval(&self, _: &str) -> Result<i64, SQLError> {
        Ok(self.0.load(Ordering::Relaxed))
    }
    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64, SQLError> {
        unreachable!("reference never sets the sequence")
    }
    fn resolve_regclass(&self, _: &str) -> Result<Option<i64>, String> {
        Ok(Some(90001))
    }
    fn require_builtin_execute(
        &self,
        binding: &uqa_sql::ast::FunctionBinding,
    ) -> Result<(), SQLError> {
        assert!(
            !matches!(binding.dispatch, Some(FunctionDispatch::Collate { .. })),
            "a collation clause is not a function call"
        );
        Ok(())
    }
}

fn expression(source: &str, schema: &RowSchema) -> ScalarExpr {
    let uqa_sql::Statement::Select(mut query) = uqa_sql::compile(&format!("SELECT {source}"))
        .unwrap()
        .remove(0)
    else {
        panic!("SELECT")
    };
    let scalar = uqa_sql::plan::ExpressionPlan::lower(query.projections.remove(0).expr).scalar;
    uqa_sql::bind_type_introspection(scalar, schema, &[])
}

fn compare(result: Result<Value, SQLError>, case: &serde_json::Value) {
    let label = case["expression"].as_str().unwrap();
    if case["error"].is_null() {
        let value = result.unwrap_or_else(|error| panic!("{label}: {error}"));
        let text = if value == Value::Null {
            None
        } else {
            Some(uqa_sql::expr::value_to_string(&value).unwrap())
        };
        assert_eq!(text.as_deref(), case["value"].as_str(), "{label}");
    } else {
        let error = result.unwrap_err();
        assert_eq!(
            error.sqlstate(),
            case["error"]["sqlstate"].as_str(),
            "{label}"
        );
        assert_eq!(
            error.to_string(),
            case["error"]["message"].as_str().unwrap(),
            "{label}"
        );
    }
}

#[test]
fn collation_value_evaluation_matches_postgresql_in_physical_contexts() {
    let cases: Vec<serde_json::Value> = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../uqa-sql/src/expr/tests/collation_values.json"
    )))
    .unwrap();
    let schema = RowSchema::default();
    let hook = Sequence::default();
    let context = ScalarEvalContext::new(None, &[]).with_function_hook(&hook);
    let budget = MemoryBudget::new(16384);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for case in cases {
        let expression = expression(case["expression"].as_str().unwrap(), &schema);
        compare(eval_scalar(&expression, &context), &case);
        if !case["requires_sequence"].as_bool().unwrap() {
            let result =
                eval_scalar_inner(&expression, &ScalarEvalContext::new(None, &[]), &control);
            compare(result.map(|value| (*value).clone()), &case);
            assert_eq!(budget.used(), 0);
        }
    }
    assert_eq!(hook.0.load(Ordering::Relaxed), 1);
}

#[test]
fn collation_clauses_forward_one_admitted_operand() {
    let schema = RowSchema::with_types(vec!["payload".into()], vec![Some(ColumnType::Text)]);
    let payload = "p".repeat(4096);
    let row = uqa_sql::ResultRow::from([("payload".into(), Value::Str(payload.clone()))]);
    let source = format!("payload{}", " COLLATE \"C\"".repeat(32));
    let expression = expression(&source, &schema);
    let context = ScalarEvalContext::from_row_lookup(&row, &[]).with_row_schema(&schema);
    let budget = MemoryBudget::new(payload.len());
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let output = eval_scalar_inner(&expression, &context, &control).unwrap();
    assert_eq!(*output, Value::Str(payload.clone()));
    assert_eq!(budget.used(), payload.len());
    assert_eq!(budget.peak(), payload.len());
    drop(output);
    assert_eq!(budget.used(), 0);
    let insufficient = MemoryBudget::new(payload.len() - 1);
    let error = eval_scalar_inner(
        &expression,
        &context,
        &ProductionControl::new(&insufficient, &original, &invoking),
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("53200"));
    assert_eq!(insufficient.used(), 0);
    for cancel_original in [false, true] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let error = eval_scalar_inner(
            &expression,
            &context,
            &ProductionControl::new(&budget, &original, &invoking),
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn collation_execution_reports_retained_errors_before_operand_effects() {
    let hook = Sequence::default();
    let context = ScalarEvalContext::new(None, &[]).with_function_hook(&hook);
    let mut expression = expression(
        "nextval('effect')::text COLLATE \"C\"",
        &RowSchema::default(),
    );
    let ScalarExpr::Func {
        binding: Some(binding),
        ..
    } = &mut expression
    else {
        panic!("clause")
    };
    binding.resolution_error = Some(Box::new(uqa_sql::ast::FunctionResolutionError::Operator(
        Box::new(uqa_sql::ast::OperatorResolutionError {
            sqlstate: "42804".into(),
            message: "retained type error".into(),
        }),
    )));
    let error = eval_scalar(&expression, &context).unwrap_err();
    assert_eq!(error.sqlstate(), Some("42804"));
    assert_eq!(error.to_string(), "retained type error");
    assert_eq!(hook.0.load(Ordering::Relaxed), 0);
}
