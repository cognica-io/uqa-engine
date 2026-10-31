//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ir::{ConstantValueSource, RetainedConstantValue, RetainedScalarLiteral},
    SQLError,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use uqa_core::{
    memory::{MemoryBudget, Produced, ProductionControl},
    CancellationToken,
};

#[derive(Debug)]
struct Source {
    value: Value,
    resident: bool,
    failure: bool,
    reads: AtomicUsize,
}

impl ConstantValueSource for Source {
    fn read(&self, _: u64) -> Result<Value, SQLError> {
        panic!("controlled identity must not call ordinary storage decoding")
    }

    fn resident_value(&self, _: u64) -> Option<&Value> {
        self.resident.then_some(&self.value)
    }

    fn read_with_control(
        &self,
        _: u64,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Value>, SQLError> {
        control.check()?;
        self.reads.fetch_add(1, Ordering::Relaxed);
        if self.failure {
            return Err(SQLError::Internal("semantic storage failure".into()));
        }
        Ok(control.copy_value(&self.value)?)
    }
}

fn source(value: Value, resident: bool, failure: bool) -> Arc<Source> {
    Arc::new(Source {
        value,
        resident,
        failure,
        reads: AtomicUsize::new(0),
    })
}

fn retained(source: Arc<Source>, declaration: Option<String>) -> ScalarExpr {
    ScalarExpr::RetainedLiteral(RetainedScalarLiteral::new(
        RetainedConstantValue::new(source, 0),
        declaration.as_ref().map(|_| crate::ColumnType::Text),
        declaration,
        None,
    ))
}

#[test]
fn resident_and_identical_retained_keys_borrow_values_and_metadata() {
    let declaration = "catalog_type".repeat(4096);
    let resident = source(Value::Str("value".repeat(4096)), true, false);
    let retained = retained(Arc::clone(&resident), Some(declaration.clone()));
    let inline = ScalarExpr::TypedLiteral {
        value: resident.value.clone(),
        ty: declaration,
        bound_type: Some(crate::ColumnType::Text),
        parameter_index: None,
        composite_source: None,
        collation_oid: None,
    };
    let budget = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let allocations = allocation_counter::measure(|| {
        assert!(exprs_match_with_control(&retained, &inline, &control).unwrap());
        assert!(exprs_match_with_control(&retained, &retained, &control).unwrap());
    });
    assert_eq!(allocations.count_total, 0);
    assert_eq!(budget.peak(), 0);
    assert_eq!(resident.reads.load(Ordering::Relaxed), 0);

    let unreadable = retained_value(source(Value::Null, false, true));
    assert!(exprs_match_with_control(&unreadable, &unreadable, &control).unwrap());
}

fn retained_value(source: Arc<Source>) -> ScalarExpr {
    retained(source, None)
}

#[test]
fn independent_retained_reads_are_owned_together_and_release_on_semantic_failure() {
    let left = source(Value::Str("a".repeat(4096)), false, false);
    let right = source(left.value.clone(), false, false);
    let lhs = retained_value(Arc::clone(&left));
    let rhs = retained_value(Arc::clone(&right));
    let budget = MemoryBudget::new(16 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let allocations = allocation_counter::measure(|| {
        assert!(exprs_match_with_control(&lhs, &rhs, &control).unwrap());
    });
    assert_eq!(budget.peak(), 8192);
    assert_eq!(budget.used(), 0);
    assert_eq!(allocations.bytes_current, 0);
    assert_eq!(left.reads.load(Ordering::Relaxed), 1);
    assert_eq!(right.reads.load(Ordering::Relaxed), 1);

    let lhs = retained_value(source(Value::Null, false, true));
    assert!(!exprs_match_with_control(&lhs, &rhs, &control).unwrap());
    assert_eq!(right.reads.load(Ordering::Relaxed), 2);
    assert_eq!(budget.used(), 0);
}

#[test]
fn retained_comparison_propagates_quota_and_both_cancellations_without_leaks() {
    let left = source(Value::Str("a".repeat(4096)), false, false);
    let right = source(left.value.clone(), false, false);
    let lhs = retained_value(left);
    let rhs = retained_value(right);
    for mode in 0..3 {
        let budget = MemoryBudget::new(5000);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let sibling = budget.reserve(71).unwrap();
        if mode == 1 {
            original.cancel();
        }
        if mode == 2 {
            invoking.cancel();
        }
        let error = exprs_match_with_control(&lhs, &rhs, &control).unwrap_err();
        assert_eq!(
            error.sqlstate(),
            Some(if mode == 0 { "53200" } else { "57014" })
        );
        assert_eq!(budget.used(), sibling.bytes());
    }
}

struct Classifier(AtomicUsize);

impl AggregateClassifier for Classifier {
    fn is_registered_aggregate(&self, _: &str) -> bool {
        panic!("native aggregate traversal must use controlled catalog observation")
    }

    fn is_registered_aggregate_with_control(
        &self,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        control.check()?;
        self.0.fetch_add(1, Ordering::Relaxed);
        Ok(name.eq_ignore_ascii_case("custom_aggregate"))
    }
}

#[test]
fn aggregate_observation_uses_controlled_catalog_without_target_vector() {
    let statement = crate::compile("SELECT ROW(custom_aggregate(1), other(2)), sum(3)")
        .unwrap()
        .remove(0);
    let crate::plan::UnifiedPlan::Query(query) = crate::plan::UnifiedPlan::lower(statement) else {
        unreachable!()
    };
    let crate::plan::RelationalPlan::QueryBlock(block) = query.root else {
        unreachable!()
    };
    let aggregates = Classifier(AtomicUsize::new(0));
    let budget = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let allocations = allocation_counter::measure(|| {
        assert!(
            contains_aggregate_with_control(&aggregates, &block.projections[0].expr, &control)
                .unwrap()
        );
        assert!(
            contains_aggregate_with_control(&aggregates, &block.projections[1].expr, &control)
                .unwrap()
        );
    });
    assert_eq!(allocations.count_total, 0);
    assert_eq!(aggregates.0.load(Ordering::Relaxed), 2);
    assert_eq!(budget.peak(), 0);
    token.cancel();
    assert_eq!(
        contains_aggregate_with_control(&aggregates, &block.projections[0].expr, &control)
            .unwrap_err()
            .sqlstate(),
        Some("57014")
    );
    assert_eq!(aggregates.0.load(Ordering::Relaxed), 2);
}
