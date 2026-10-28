//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug)]
struct Source {
    value: Value,
    reads: AtomicUsize,
}
impl ConstantValueSource for Source {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut uqa_core::memory::AllocationObserver<'a, '_>,
    ) -> Result<(), uqa_core::ValueRetentionError> {
        observer.allocation(size_of::<Self>(), size_of::<Self>())?;
        observer.child(&self.value)
    }

    fn read(&self, index: u64) -> Result<Value, SQLError> {
        assert_eq!(index, 0);
        self.reads.fetch_add(1, Ordering::Relaxed);
        Ok(self.value.clone())
    }
}

#[test]
fn planning_memory_observes_shared_constants_without_reading_their_values() {
    use uqa_core::memory::{AllocationUsage, ProductionControl};
    let (expression, source) = literal(Value::Str("x".repeat(1 << 20)), ColumnType::Text);
    let one = AllocationUsage::measure(&expression, &ProductionControl::uncontrolled()).unwrap();
    let copies = vec![expression.clone(); 128];
    let observed = AllocationUsage::measure(&copies, &ProductionControl::uncontrolled()).unwrap();
    let crate::ScalarExpr::RetainedLiteral(literal) = &expression else {
        panic!("retained literal")
    };
    let metadata =
        size_of::<RetainedScalarLiteral>() + literal.declaration.as_ref().unwrap().capacity();
    let copied_metadata: usize = copies
        .iter()
        .map(|copy| {
            let crate::ScalarExpr::RetainedLiteral(literal) = copy else {
                panic!("retained literal")
            };
            size_of::<RetainedScalarLiteral>() + literal.declaration.as_ref().unwrap().capacity()
        })
        .sum();
    assert_eq!(
        observed.allocated_bytes,
        one.allocated_bytes - metadata
            + copied_metadata
            + copies.capacity() * size_of::<crate::ScalarExpr>()
    );
    assert_eq!(source.reads.load(Ordering::Relaxed), 0);
}

fn literal(value: Value, ty: ColumnType) -> (crate::ScalarExpr, Arc<Source>) {
    let source = Arc::new(Source {
        value,
        reads: AtomicUsize::new(0),
    });
    (
        crate::ScalarExpr::RetainedLiteral(Box::new(RetainedScalarLiteral {
            value: RetainedConstantValue::new(source.clone(), 0),
            declaration: Some(ty.catalog_name()),
            ty,
            collation_oid: None,
        })),
        source,
    )
}

#[test]
fn retained_constant_plan_clones_do_not_copy_or_decode_payloads() {
    let (original, source) = literal(Value::Str("x".repeat(1_048_576)), ColumnType::Text);
    let measured = allocation_counter::measure(|| {
        let copies = vec![original.clone(); 256];
        assert_eq!(copies.len(), 256);
        assert!(copies.iter().all(|copy| copy == &original));
    });
    assert_eq!(source.reads.load(Ordering::Relaxed), 0);
    assert!(measured.bytes_max < 262_144, "{} bytes", measured.bytes_max);
    assert_eq!(
        crate::scalar_type(&original, &crate::RowSchema::default(), &[]).unwrap(),
        Some(ColumnType::Text)
    );
}

#[test]
fn retained_literal_serialization_and_grouping_preserve_value_representation() {
    let (positive, _) = literal(Value::Float(0.0), ColumnType::DoublePrecision);
    let (negative, _) = literal(Value::Float(-0.0), ColumnType::DoublePrecision);
    assert_ne!(positive, negative);
    assert!(!crate::semantics::aggregates::exprs_match(
        &positive, &negative
    ));
    let (mut original, _) = literal(Value::Str("λ".repeat(1024)), ColumnType::Text);
    let crate::ScalarExpr::RetainedLiteral(stored) = &mut original else {
        unreachable!()
    };
    stored.collation_oid = Some(950);
    let ordinary = stored.materialize().unwrap();
    let bytes = serde_json::to_vec(&original).unwrap();
    let decoded: crate::ScalarExpr = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(bytes, serde_json::to_vec(&ordinary).unwrap());
    assert_eq!(ordinary, decoded);
    assert!(crate::semantics::aggregates::exprs_match(
        &original, &decoded
    ));
    assert!(crate::semantics::aggregates::exprs_match(
        &decoded, &ordinary
    ));
    assert_eq!(
        crate::schema::ScalarTypeSchema::input_collation(&crate::RowSchema::default(), &decoded),
        Some(950)
    );
}

#[test]
fn retained_constant_reads_use_the_invoking_statement_cancellation() {
    let (expression, source) = literal(Value::Str("x".repeat(1024)), ColumnType::Text);
    let crate::ScalarExpr::RetainedLiteral(literal) = expression else {
        unreachable!()
    };
    let token = uqa_core::CancellationToken::new();
    {
        let _scope = crate::expr::StatementCancellationScope::enter(token.clone());
        token.cancel();
        assert_eq!(literal.value.read().unwrap_err().sqlstate(), Some("57014"));
        assert_eq!(source.reads.load(Ordering::Relaxed), 0);
    }
    assert_eq!(literal.value.read().unwrap(), Value::Str("x".repeat(1024)));
}

#[test]
fn retained_grouping_identity_matches_the_original_literal_encoding() {
    struct Catalog;
    impl crate::FunctionTypeResolver for Catalog {
        fn resolve_function_type(
            &self,
            _: &str,
            _: Option<&crate::ast::FunctionBinding>,
            _: &[Option<String>],
            _: &[Option<ColumnType>],
            _: bool,
        ) -> Result<Option<ColumnType>, SQLError> {
            Ok(None)
        }
    }
    for (value, ty) in [
        (Value::Float(-0.0), ColumnType::DoublePrecision),
        (Value::Str("x".repeat(1024)), ColumnType::Text),
    ] {
        let (retained, _) = literal(value, ty);
        let crate::ScalarExpr::RetainedLiteral(literal) = &retained else {
            unreachable!()
        };
        let ordinary = literal.materialize().unwrap();
        let identity = |expression| {
            crate::semantics::grouping_sets::expression_identity(
                &Catalog,
                expression,
                &crate::RowSchema::default(),
                &[],
            )
            .unwrap()
        };
        assert_eq!(identity(&retained), identity(&ordinary));
    }
}
