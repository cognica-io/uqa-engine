//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::query::projection::{owned_physical_projection, owned_projection_column};
use crate::{relational::DefaultExpressionEvaluator, scan::TableScan};
use uqa_core::{memory::MemoryBudget, CancellationToken, Value};

fn source() -> Box<dyn PhysicalOperator> {
    Box::new(TableScan::from_physical_rows(
        RowSchema::new(Vec::new()),
        Vec::new(),
    ))
}
fn expression() -> ScalarExpr {
    ScalarExpr::Cast {
        expr: Box::new(ScalarExpr::Literal(Value::Str("x".repeat(4 << 20)))),
        ty: "text".into(),
        implicit: false,
    }
}

#[test]
fn physical_projection_keeps_only_live_expression_and_schema_owners() {
    let expression = expression();
    let budget = MemoryBudget::new(32 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let input = || {
        owned_physical_projection(
            owned_projection_column("payload", &control)?,
            &expression,
            true,
            &control,
        )
    };
    let project = Project::with_owned_target_evaluator(
        source(),
        std::iter::once_with(input),
        DefaultExpressionEvaluator::shared(Vec::new()),
        &control,
    )
    .unwrap();
    assert!(budget.used() >= 4 << 20);
    let output = project.row_schema().clone();
    assert_eq!(output.columns(), &["payload"]);
    assert_eq!(output.column_type(0), Some(&ColumnType::Text));
    drop(project);
    assert!(budget.used() > 0 && budget.used() < 64 << 10);
    drop(output);
    assert_eq!(budget.used(), 0);
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _project = Project::with_owned_target_evaluator(
            source(),
            std::iter::once_with(input),
            DefaultExpressionEvaluator::shared(Vec::new()),
            &control,
        )
        .unwrap();
        panic!("release physical expression owners");
    }));
    assert!(panic.is_err());
    assert_eq!(budget.used(), 0);
}

#[test]
fn physical_projection_rejects_before_copying_and_consumes_cancelled_inputs() {
    let expression = expression();
    let budget = MemoryBudget::new(32 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let input = || {
        owned_physical_projection(
            owned_projection_column("payload", &control)?,
            &expression,
            true,
            &control,
        )
    };
    let occupied = budget.reserve(budget.available() - (64 << 10)).unwrap();
    let mut error = None;
    let allocation = allocation_counter::measure(|| {
        error = Some(
            Project::with_owned_target_evaluator(
                source(),
                std::iter::once_with(input),
                DefaultExpressionEvaluator::shared(Vec::new()),
                &control,
            )
            .err()
            .unwrap(),
        );
    });
    assert!(allocation.bytes_total < 256 << 10, "{allocation:?}");
    assert_eq!(error.unwrap().sqlstate(), Some("53200"));
    assert_eq!(budget.used(), occupied.bytes());
    drop(occupied);
    for token in [&original, &invoking] {
        let projection = input().unwrap();
        token.cancel();
        let error = Project::with_owned_target_evaluator(
            source(),
            std::iter::once(Ok(projection)),
            DefaultExpressionEvaluator::shared(Vec::new()),
            &control,
        )
        .err()
        .unwrap();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(budget.used(), 0);
        token.reset();
    }
}

#[test]
fn physical_projection_collects_then_binds_all_inputs_before_layout_type_observation() {
    use std::sync::{Arc, Mutex};
    struct Evaluator(Mutex<Vec<&'static str>>);
    impl crate::ExpressionEvaluator for Evaluator {
        fn evaluate(
            &self,
            _: &ScalarExpr,
            _: &dyn uqa_sql::expr::RowLookup,
        ) -> crate::ExecResult<Value> {
            unreachable!("construction must not evaluate rows")
        }
        fn bind_type_introspection_owned(
            &self,
            expression: OwnedScalarExpr,
            _: &RowSchema,
            _: &ProductionControl<'_>,
        ) -> Result<OwnedScalarExpr, SQLError> {
            self.0.lock().unwrap().push("bind");
            if matches!(&*expression, ScalarExpr::Literal(Value::Int(2))) {
                return Err(SQLError::TypeMismatch("later binding error".into()));
            }
            Ok(expression)
        }
        fn expression_type_with_control(
            &self,
            _: &ScalarExpr,
            _: &RowSchema,
            control: &ProductionControl<'_>,
        ) -> Result<Option<Produced<ColumnType>>, SQLError> {
            self.0.lock().unwrap().push("type");
            Ok(Some(control.finish(
                ColumnType::Integer,
                control.empty_reservation(),
            )?))
        }
    }
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let evaluator = Arc::new(Evaluator(Mutex::new(Vec::new())));
    let input = |value| {
        owned_physical_projection(
            owned_projection_column("value", &control)?,
            &ScalarExpr::Literal(Value::Int(value)),
            false,
            &control,
        )
    };
    let error = Project::with_owned_target_evaluator(
        source(),
        [
            input(1),
            Err(SQLError::TypeMismatch("later input error".into())),
        ],
        evaluator.clone(),
        &control,
    )
    .err()
    .unwrap();
    assert!(error.to_string().contains("later input error"));
    assert!(evaluator.0.lock().unwrap().is_empty());
    assert_eq!(budget.used(), 0);
    let error = Project::with_owned_target_evaluator(
        source(),
        [input(1), input(2)],
        evaluator.clone(),
        &control,
    )
    .err()
    .unwrap();
    assert!(error.to_string().contains("later binding error"));
    assert_eq!(*evaluator.0.lock().unwrap(), ["bind", "bind"]);
    assert_eq!(budget.used(), 0);
    evaluator.0.lock().unwrap().clear();
    let project = Project::with_owned_target_evaluator(
        source(),
        [input(1), input(3)],
        evaluator.clone(),
        &control,
    )
    .unwrap();
    assert_eq!(
        *evaluator.0.lock().unwrap(),
        ["bind", "bind", "type", "type"]
    );
    drop(project);
    assert_eq!(budget.used(), 0);
}
