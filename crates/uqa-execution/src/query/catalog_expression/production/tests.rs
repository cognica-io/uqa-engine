//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::query::relational::values::test_support::Services;
use uqa_core::{memory::MemoryBudget, CancellationToken};
use uqa_sql::{ast::ColumnType, ScalarExpr};

#[test]
fn catalog_scalar_retires_preparation_and_retains_only_the_selected_output() {
    let input = "catalog value".repeat(512);
    let row = uqa_sql::ResultRow::from([("value".into(), Value::Str(input.clone()))]);
    let schema = RowSchema::with_types(vec!["value".into()], vec![Some(ColumnType::Text)]);
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let scalar = ScalarExpr::Column("value".into())
        .copy_for_rewrite(&control)
        .unwrap();
    let mut optimizations = 0;
    let output = evaluate_catalog_scalar_with_control(
        scalar,
        &row,
        &schema,
        &[],
        &Services,
        &control,
        |scalar| {
            optimizations += 1;
            Ok(scalar)
        },
    )
    .unwrap();
    assert_eq!(*output, Value::Str(input));
    assert_eq!(optimizations, 1);
    assert_eq!(memory.used(), output.reserved_bytes());
    drop(output);
    assert_eq!(memory.used(), 0);
}

#[test]
fn catalog_scalar_keeps_lazy_errors_and_releases_cancelled_preparation() {
    use uqa_sql::ast::BinaryOp;
    let scalar = ScalarExpr::Case {
        base: None,
        when: vec![(
            ScalarExpr::Literal(Value::Bool(true)),
            ScalarExpr::Literal(Value::Int(7)),
        )],
        else_branch: Some(Box::new(ScalarExpr::Binary {
            op: BinaryOp::Divide,
            lhs: Box::new(ScalarExpr::Literal(Value::Int(1))),
            rhs: Box::new(ScalarExpr::Literal(Value::Int(0))),
        })),
    };
    let row = uqa_sql::ResultRow::new();
    let schema = RowSchema::default();
    let memory = MemoryBudget::new(1 << 20);
    for cancel in [None, Some(false), Some(true)] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&memory, &original, &invoking);
        let scalar = scalar.copy_for_rewrite(&control).unwrap();
        if let Some(cancel_original) = cancel {
            if cancel_original {
                original.cancel();
            } else {
                invoking.cancel();
            }
        }
        let output = evaluate_catalog_scalar_with_control(
            scalar,
            &row,
            &schema,
            &[],
            &Services,
            &control,
            Ok,
        );
        if cancel.is_some() {
            assert_eq!(output.unwrap_err().sqlstate(), Some("57014"));
        } else {
            assert_eq!(*output.unwrap(), Value::Int(7));
        }
        assert_eq!(memory.used(), 0);
    }
}

mod preparation {
    use crate::query::{
        catalog_expression::PreparedCatalogExpression, relational::QueryExpressionFactory, CteScope,
    };
    use crate::RowSchema;
    use std::sync::Arc;
    use uqa_core::Value;
    use uqa_sql::{ResultRow, SQLError, SQLParam};

    use std::sync::atomic::{AtomicUsize, Ordering};
    use uqa_core::{
        memory::{MemoryBudget, OwnedProductionControl, ProductionControl},
        CancellationToken,
    };

    struct Factory {
        resources: OwnedProductionControl,
        bindings: AtomicUsize,
    }

    impl QueryExpressionFactory<()> for Factory {
        fn production_resources(&self) -> Result<OwnedProductionControl, SQLError> {
            Ok(self.resources.clone())
        }
        fn bind_scope(
            &self,
            _: &CteScope,
        ) -> Result<Arc<dyn crate::query::set_projection::SetFunctionRuntime + '_>, SQLError>
        {
            self.bindings.fetch_add(1, Ordering::SeqCst);
            Ok(Arc::new(
                crate::query::relational::values::test_support::Services,
            ))
        }
        fn optimize_owned_catalog_scalar(
            &self,
            expression: uqa_sql::plan::OwnedScalarExpr,
            _control: &uqa_core::memory::ProductionControl<'_>,
        ) -> Result<uqa_sql::plan::OwnedScalarExpr, SQLError> {
            Ok(expression)
        }

        fn optimize_catalog_scalar(&self, _: &mut crate::ScalarExpr) -> Result<(), SQLError> {
            Ok(())
        }
        fn prepare_predicate<'a>(
            &'a self,
            _: &crate::ScalarExpr,
            _: &'a [SQLParam],
            _: &CteScope,
        ) -> Result<Option<crate::SharedRowPredicate<'a>>, SQLError> {
            unreachable!("catalog expression preparation has no query predicate")
        }
    }

    #[test]
    fn prepared_catalog_expression_admits_borrowed_syntax_before_binding() {
        let memory = MemoryBudget::new(32 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&memory, &original, &invoking);
        let factory = Factory {
            resources: control.capture(),
            bindings: AtomicUsize::new(0),
        };
        let source = uqa_sql::ast::Expr::Literal(Value::Str("x".repeat(4 << 20)));
        let schema = RowSchema::default();
        let prepare = || {
            PreparedCatalogExpression::lower(&factory, CteScope::default(), &source, &schema, &[])
        };
        let occupied = memory.reserve(memory.available() - (64 << 10)).unwrap();
        let allocation = allocation_counter::measure(|| {
            assert_eq!(
                prepare()
                    .err()
                    .expect("admit scalar destination before binding")
                    .sqlstate(),
                Some("53200")
            );
        });
        assert!(allocation.bytes_total < 128 << 10, "{allocation:?}");
        assert_eq!(factory.bindings.load(Ordering::SeqCst), 0);
        assert_eq!(memory.used(), occupied.bytes());
        drop(occupied);
        let prepared = prepare().unwrap();
        assert!(memory.used() >= 4 << 20);
        let row = ResultRow::new();
        let retained = memory.used();
        for token in [&original, &invoking] {
            token.cancel();
            assert_eq!(
                prepared.evaluate(&row).unwrap_err().sqlstate(),
                Some("57014")
            );
            assert_eq!(memory.used(), retained);
            token.reset();
        }
        let (value, _) = prepared.evaluate(&row).unwrap();
        let Value::Str(value) = value else {
            panic!("literal value")
        };
        assert_eq!(value.len(), 4 << 20);
        assert!(value.bytes().all(|byte| byte == b'x'));
        drop((value, prepared));
        assert_eq!(memory.used(), 0);
        for token in [&original, &invoking] {
            token.cancel();
            assert_eq!(prepare().err().unwrap().sqlstate(), Some("57014"));
            assert_eq!(memory.used(), 0);
            token.reset();
        }
        drop(prepare().unwrap());
        assert_eq!(memory.used(), 0);
    }
}
