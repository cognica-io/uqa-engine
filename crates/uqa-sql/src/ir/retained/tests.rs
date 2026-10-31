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
fn resident_constant_comparison_and_plan_copy_do_not_allocate_or_release_the_owner() {
    use uqa_core::{memory::MemoryBudget, CancellationToken};
    let memory = MemoryBudget::new(16 * 1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let input = Value::Str("x".repeat(4 * 1024 * 1024));
    let make = || {
        RetainedScalarLiteral::with_control(
            RetainedConstantValue::from_produced(control.copy_value(&input).unwrap(), &control)
                .unwrap(),
            Some(ColumnType::Text.clone_with_control(&control).unwrap()),
            Some(control.copy_text("text").unwrap()),
            None,
            &control,
        )
        .unwrap()
    };
    let left = crate::ScalarExpr::RetainedLiteral(make());
    let right = crate::ScalarExpr::RetainedLiteral(make());
    let held = memory.used();
    let empty = MemoryBudget::new(0);
    let copying = ProductionControl::new(&empty, &token, &token);
    let mut copy = None;
    let allocations = allocation_counter::measure(|| {
        assert_eq!(left, right);
        copy = Some(left.clone_with_control(&copying).unwrap());
    });
    assert_eq!(allocations.bytes_total, 0, "{allocations:?}");
    assert_eq!(empty.used(), 0);
    assert_eq!(memory.used(), held);
    drop((left, right));
    assert!(memory.used() > 4 * 1024 * 1024);
    drop(copy);
    assert_eq!(memory.used(), 0);
}

#[test]
fn planning_memory_observes_shared_constants_without_reading_their_values() {
    use uqa_core::memory::{AllocationUsage, ProductionControl};
    let (expression, source) = literal(Value::Str("x".repeat(1 << 20)), ColumnType::Text);
    let one = AllocationUsage::measure(&expression, &ProductionControl::uncontrolled()).unwrap();
    let copies = vec![expression.clone(); 128];
    let observed = AllocationUsage::measure(&copies, &ProductionControl::uncontrolled()).unwrap();
    assert_eq!(
        observed.allocated_bytes,
        one.allocated_bytes + copies.capacity() * size_of::<crate::ScalarExpr>()
    );
    assert_eq!(source.reads.load(Ordering::Relaxed), 0);
}

fn literal(value: Value, ty: ColumnType) -> (crate::ScalarExpr, Arc<Source>) {
    let source = Arc::new(Source {
        value,
        reads: AtomicUsize::new(0),
    });
    (
        crate::ScalarExpr::RetainedLiteral(RetainedScalarLiteral::new(
            RetainedConstantValue::new(source.clone(), 0),
            Some(ty.clone()),
            Some(ty.catalog_name()),
            None,
        )),
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
    *stored = RetainedScalarLiteral::new(
        stored.value.clone(),
        stored.ty.clone(),
        stored.declaration.clone(),
        Some(950),
    );
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
