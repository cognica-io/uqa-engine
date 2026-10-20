//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::ColumnType,
    ir::{ConstantValueSource, ConstantValueStorage, RetainedConstantValue},
    plan::ExpressionPlan,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use uqa_core::{memory::MemoryBudget, CancellationToken, Value};

#[derive(Debug)]
struct Source {
    value: Value,
    reads: AtomicUsize,
}
impl ConstantValueSource for Source {
    fn read(&self, index: u64) -> Result<Value, SQLError> {
        assert_eq!(index, 0);
        self.reads.fetch_add(1, Ordering::Relaxed);
        Ok(self.value.clone())
    }
}
#[derive(Debug)]
struct Storage;
impl ConstantValueStorage for Storage {
    fn retain(&self, value: Value) -> Result<RetainedConstantValue, SQLError> {
        Ok(RetainedConstantValue::new(
            Arc::new(Source {
                value,
                reads: AtomicUsize::new(0),
            }),
            0,
        ))
    }
}

fn typed(value: Value, ty: &str) -> Expr {
    Expr::TypedLiteral {
        value,
        ty: ty.into(),
        collation_oid: Some(950),
        composite_source: None,
    }
}

#[test]
fn retained_input_cloning_and_lowering_share_payloads_with_admitted_metadata() {
    let source = Arc::new(Source {
        value: Value::Str("x".repeat(1_048_576)),
        reads: AtomicUsize::new(0),
    });
    let expression = Expr::RetainedLiteral(Box::new(RetainedInputLiteral(RetainedScalarLiteral {
        value: RetainedConstantValue::new(source.clone(), 0),
        ty: ColumnType::Text,
        declaration: Some(ColumnType::Text.catalog_name()),
        collation_oid: Some(950),
    })));
    let token = CancellationToken::new();
    let budget = MemoryBudget::new(262_144);
    let measured = allocation_counter::measure(|| {
        let copies: Vec<_> = (0..256)
            .map(|_| ExpressionPlan::lower(expression.clone()).scalar)
            .collect();
        assert_eq!(copies.len(), 256);
        let borrowed =
            ExpressionPlan::lower_column_budgeted(&expression, &budget, &token, &token).unwrap();
        assert!(borrowed.reserved_bytes() > 0);
        assert!(matches!(&*borrowed, crate::ScalarExpr::RetainedLiteral(_)));
    });
    assert_eq!(source.reads.load(Ordering::Relaxed), 0);
    assert!(measured.bytes_max < 262_144, "{} bytes", measured.bytes_max);
    assert!(ExpressionPlan::lower_column_budgeted(
        &expression,
        &MemoryBudget::new(1),
        &token,
        &token
    )
    .is_err());
    assert_eq!(source.reads.load(Ordering::Relaxed), 0);
    token.cancel();
    let _scope = crate::expr::StatementCancellationScope::enter(token);
    assert_eq!(
        crate::expr::eval(&expression, &crate::expr::EvalContext::new(None, &[]))
            .unwrap_err()
            .sqlstate(),
        Some("57014")
    );
    assert_eq!(source.reads.load(Ordering::Relaxed), 0);
}

#[test]
fn retained_input_preserves_serialization_type_collation_and_value_representation() {
    for ordinary in [
        typed(
            Value::Str("λ".repeat(2048)),
            &ColumnType::Text.catalog_name(),
        ),
        Expr::Literal(Value::Str("z".repeat(4096))),
    ] {
        let retained = RetainedInputLiteral::retain(ordinary.clone(), &Storage).unwrap();
        assert!(matches!(&retained, Expr::RetainedLiteral(_)));
        let bytes = serde_json::to_vec(&retained).unwrap();
        assert_eq!(bytes, serde_json::to_vec(&ordinary).unwrap());
        assert_eq!(serde_json::from_slice::<Expr>(&bytes).unwrap(), ordinary);
        assert_eq!(
            crate::expr::eval(&retained, &crate::expr::EvalContext::new(None, &[])).unwrap(),
            crate::expr::eval(&ordinary, &crate::expr::EvalContext::new(None, &[])).unwrap()
        );
        let scalar = ExpressionPlan::lower(retained).scalar;
        assert_eq!(
            crate::scalar_type(&scalar, &crate::RowSchema::default(), &[]).unwrap(),
            Some(ColumnType::Text)
        );
        if matches!(ordinary, Expr::TypedLiteral { .. }) {
            assert_eq!(
                crate::schema::ScalarTypeSchema::input_collation(
                    &crate::RowSchema::default(),
                    &scalar
                ),
                Some(950)
            );
        }
    }
}

#[test]
fn retained_input_keeps_catalog_types_and_inline_literals_on_existing_paths() {
    for ordinary in [
        typed(Value::Str("x".repeat(4096)), "public.domain_text"),
        Expr::Literal(Value::Int(42)),
        Expr::Literal(Value::Null),
    ] {
        assert_eq!(
            RetainedInputLiteral::retain(ordinary.clone(), &Storage).unwrap(),
            ordinary
        );
    }
}
