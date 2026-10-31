//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::plan::CommandPlan;
use uqa_core::memory::ProductionControl;

fn capture(params: &[SQLParam]) -> AnalyzedStatement {
    AnalyzedStatement::capture_with_control(
        &UnifiedPlan::Command(Box::new(CommandPlan::Empty)),
        params,
        &ProductionControl::uncontrolled(),
    )
    .unwrap()
    .unwrap()
}

#[test]
fn analyzed_cache_matching_borrows_large_declared_types_and_checks_arity_first() {
    let params = [SQLParam::TypedScalar {
        value: Value::Null,
        ty: ColumnType::Array(Box::new(ColumnType::Named("t".repeat(4 * 1024 * 1024)))),
    }];
    let cached = capture(&params);
    let empty = capture(&[]);
    let allocation = allocation_counter::measure(|| {
        assert!(cached.plan_for(&params).is_some());
        assert!(empty.plan_for(&params).is_none());
        assert!(cached.plan_for(&[]).is_none());
    });
    assert_eq!(allocation.count_total, 0, "{allocation:?}");
}

#[test]
fn analyzed_cache_matching_observes_inferred_arrays_without_building_types() {
    let params = [SQLParam::Scalar(Value::List(
        (0..2048)
            .map(|index| Value::List(vec![Value::Int(index), Value::Null]))
            .collect(),
    ))];
    let cached = capture(&params);
    let different = [SQLParam::Scalar(Value::List(vec![Value::List(vec![
        Value::Int(i64::MAX),
    ])]))];
    let allocation = allocation_counter::measure(|| {
        assert!(cached.plan_for(&params).is_some());
        assert!(cached.plan_for(&different).is_none());
    });
    assert_eq!(allocation.count_total, 0, "{allocation:?}");
}

#[test]
fn analyzed_cache_matching_streams_wide_parameter_lists_and_keeps_declarations_distinct() {
    let params = vec![SQLParam::Scalar(Value::Int(1)); 8192];
    let cached = capture(&params);
    let declared = [SQLParam::typed_scalar(Value::Null, ColumnType::Text)];
    let deferred = [SQLParam::DeferredError {
        ty: ColumnType::Text,
        error: Box::new(crate::ast::DeferredSQLError::from(&SQLError::TypeMismatch(
            "must remain deferred".into(),
        ))),
    }];
    let untyped = [SQLParam::Scalar(Value::Str("text".into()))];
    let declared_cache = capture(&declared);
    let inferred_cache = capture(&untyped);
    let allocation = allocation_counter::measure(|| {
        assert!(cached.plan_for(&params).is_some());
        assert!(declared_cache.plan_for(&deferred).is_some());
        assert!(declared_cache.plan_for(&untyped).is_none());
        assert!(inferred_cache.plan_for(&declared).is_none());
        assert!(inferred_cache.plan_for(&deferred).is_none());
    });
    assert_eq!(allocation.count_total, 0, "{allocation:?}");
}

#[test]
fn analyzed_cache_matching_preserves_unknown_arrays_and_empty_tensor_dimensions() {
    let unknown = capture(&[SQLParam::Scalar(Value::Null)]);
    let incompatible = [SQLParam::Scalar(Value::List(vec![
        Value::Int(1),
        Value::Bool(false),
    ]))];
    let empty = [SQLParam::Tensor(Vec::new())];
    let tensor = |dimensions| {
        SQLParam::TypedTensor(
            crate::params::SQLTensor::from_slice_with_control(
                &[],
                dimensions,
                &ProductionControl::uncontrolled(),
            )
            .unwrap()
            .into_uncontrolled()
            .unwrap(),
        )
    };
    let typed = [tensor(3)];
    let other_dimension = [tensor(4)];
    let rows = [SQLParam::Tensor(vec![vec![1.0, 2.0, 3.0]])];
    let typed_cache = capture(&typed);
    let allocation = allocation_counter::measure(|| {
        assert!(unknown.plan_for(&incompatible).is_some());
        assert!(unknown.plan_for(&empty).is_some());
        assert!(unknown.plan_for(&typed).is_none());
        assert!(typed_cache.plan_for(&typed).is_some());
        assert!(typed_cache.plan_for(&rows).is_some());
        assert!(typed_cache.plan_for(&other_dimension).is_none());
    });
    assert_eq!(allocation.count_total, 0, "{allocation:?}");
}
