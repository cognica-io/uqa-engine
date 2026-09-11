//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::test_support::{materialized, plan, Services};
use super::*;
use crate::query::scope::subqueries::{CachedCorrelatedExists, CorrelatedExistsOuterKeys};
use crate::query::CteScope;
use crate::{CanonicalRowHashSet, PhysicalRow, RowSchema, ScalarExpr};

#[test]
fn cached_consumers_skip_metadata_memory_and_query_contexts() {
    let services = Services::default();
    let scope = CteScope::new();
    let query = plan("SELECT 1");
    scope.cache_subquery(0, ScalarSubqueryCacheEntry::Scalar(Value::Int(7)));
    scope.cache_subquery(1, ScalarSubqueryCacheEntry::Exists(true));
    scope.cache_subquery(
        2,
        ScalarSubqueryCacheEntry::Materialized(materialized(vec![Value::Int(9)], 1)),
    );
    let context = services.context(&scope);
    assert_eq!(
        context
            .scalar_subquery_value(0, &query, PhysicalOuterRow::Absent, &[])
            .unwrap(),
        Value::Int(7)
    );
    assert!(context
        .subquery_exists(1, &query, PhysicalOuterRow::Absent, &[])
        .unwrap());
    assert_eq!(
        context
            .execute_subquery(2, &query, PhysicalOuterRow::Absent, &[])
            .unwrap()
            .into_scalar_value()
            .unwrap(),
        Value::Int(9)
    );
    assert!(context
        .subquery_exists(2, &query, PhysicalOuterRow::Absent, &[])
        .unwrap());
    assert!(services.events.lock().is_empty());
}

#[test]
fn a_slot_cannot_switch_to_an_incompatible_result_consumer() {
    let services = Services::default();
    let scope = CteScope::new();
    let query = plan("SELECT 1");
    scope.cache_subquery(0, ScalarSubqueryCacheEntry::Scalar(Value::Int(7)));
    scope.cache_subquery(1, ScalarSubqueryCacheEntry::Exists(true));
    let context = services.context(&scope);
    let errors = [
        context
            .execute_subquery(0, &query, PhysicalOuterRow::Absent, &[])
            .err()
            .unwrap(),
        context
            .subquery_exists(0, &query, PhysicalOuterRow::Absent, &[])
            .unwrap_err(),
        context
            .subquery_contains(0, &query, &Value::Int(1), PhysicalOuterRow::Absent, &[])
            .unwrap_err(),
        context
            .scalar_subquery_value(1, &query, PhysicalOuterRow::Absent, &[])
            .unwrap_err(),
    ];
    for error in errors {
        assert!(
            matches!(error, SQLError::Internal(message) if message == "scalar subquery slot changed result consumer during execution")
        );
    }
    assert!(services.events.lock().is_empty());
}

#[test]
fn failed_membership_promotion_preserves_materialized_rows() {
    let services = Services {
        fail_memory: true,
        ..Services::default()
    };
    let scope = CteScope::new();
    let query = plan("SELECT 1");
    scope.cache_subquery(
        0,
        ScalarSubqueryCacheEntry::Materialized(materialized(vec![Value::Int(7)], 1)),
    );
    let error = services
        .context(&scope)
        .subquery_contains(0, &query, &Value::Int(7), PhysicalOuterRow::Absent, &[])
        .unwrap_err();
    assert!(
        matches!(error, SQLError::Internal(message) if message == "memory setting unavailable")
    );
    assert!(matches!(
        scope.cached_subquery(0),
        Some(ScalarSubqueryCacheEntry::Materialized(_))
    ));
    assert_eq!(*services.events.lock(), ["memory"]);
    assert_eq!(
        services
            .context(&scope)
            .scalar_subquery_value(0, &query, PhysicalOuterRow::Absent, &[])
            .unwrap(),
        Value::Int(7)
    );
}

#[test]
fn membership_promotion_preserves_null_semantics_and_reuses_the_set() {
    for budget in [1, usize::MAX] {
        let services = Services::default();
        let scope = CteScope::new();
        let query = plan("SELECT 1");
        scope.cache_subquery(
            0,
            ScalarSubqueryCacheEntry::Materialized(materialized(
                vec![Value::Int(7), Value::Null],
                budget,
            )),
        );
        let context = services.context(&scope);
        for (needle, expected) in [
            (Value::Int(7), Some(true)),
            (Value::Int(8), None),
            (Value::Null, None),
        ] {
            assert_eq!(
                context
                    .subquery_contains(0, &query, &needle, PhysicalOuterRow::Absent, &[])
                    .unwrap(),
                expected
            );
        }
        assert!(matches!(
            scope.cached_subquery(0),
            Some(ScalarSubqueryCacheEntry::Membership(_))
        ));
        assert_eq!(*services.events.lock(), ["memory"]);
    }
}

#[test]
fn scalar_cardinality_error_does_not_replace_the_materialized_cache() {
    let services = Services::default();
    let scope = CteScope::new();
    let query = plan("SELECT 1");
    scope.cache_subquery(
        0,
        ScalarSubqueryCacheEntry::Materialized(materialized(vec![Value::Int(1), Value::Int(2)], 1)),
    );
    let error = services
        .context(&scope)
        .scalar_subquery_value(0, &query, PhysicalOuterRow::Absent, &[])
        .unwrap_err();
    assert!(matches!(error, SQLError::Routine { sqlstate, .. } if sqlstate == "21000"));
    assert!(matches!(
        scope.cached_subquery(0),
        Some(ScalarSubqueryCacheEntry::Materialized(_))
    ));
    assert!(services.events.lock().is_empty());
}

#[test]
fn correlation_is_cached_before_a_missing_outer_row_fails() {
    let mut services = Services {
        allow_metadata: true,
        ..Services::default()
    };
    let scope = CteScope::new();
    let query = plan("SELECT outer_value");
    for first in [true, false] {
        let error = services
            .context(&scope)
            .scalar_subquery_value(0, &query, PhysicalOuterRow::Absent, &[])
            .unwrap_err();
        assert!(
            matches!(error, SQLError::Internal(message) if message == "correlated subquery reached execution without a positional outer row")
        );
        assert!(matches!(
            scope.cached_subquery(0),
            Some(ScalarSubqueryCacheEntry::Correlated)
        ));
        assert_eq!(
            *services.events.lock(),
            if first {
                vec!["catalog", "resolution"]
            } else {
                Vec::new()
            }
        );
        services.events.lock().clear();
        services.allow_metadata = false;
    }
}

fn cache_keys(scope: &CteScope, expressions: Vec<ScalarExpr>) {
    let mut keys = CanonicalRowHashSet::new();
    keys.insert_values(&[Value::Int(7)]).unwrap();
    scope.cache_subquery(
        0,
        ScalarSubqueryCacheEntry::CorrelatedExists(Arc::new(CachedCorrelatedExists {
            outer_keys: CorrelatedExistsOuterKeys::compile(expressions),
            keys,
        })),
    );
}

#[test]
fn direct_exists_keys_resolve_qualified_columns_and_skip_missing_or_null_values() {
    let services = Services::default();
    let scope = CteScope::new();
    let query = plan("SELECT 1");
    cache_keys(
        &scope,
        vec![ScalarExpr::QualifiedColumn {
            qualifier: "outer_row".into(),
            column: "id".into(),
        }],
    );
    for (column, value, expected) in [
        ("id", Value::Int(7), true),
        ("id", Value::Int(8), false),
        ("id", Value::Null, false),
        ("other", Value::Int(7), false),
    ] {
        let schema = RowSchema::with_qualified_types("outer_row", vec![column.into()], vec![None]);
        let row = PhysicalRow::from_values(vec![value]);
        assert_eq!(
            services
                .context(&scope)
                .subquery_exists(
                    0,
                    &query,
                    PhysicalOuterRow::Physical {
                        schema: &schema,
                        row: &row
                    },
                    &[]
                )
                .unwrap(),
            expected
        );
    }
    assert!(services.events.lock().is_empty());
}

#[test]
fn evaluated_exists_keys_use_scoped_function_and_subquery_callbacks() {
    let services = Services::default();
    let mut scope = CteScope::new();
    let query = plan("SELECT 1");
    scope.scalar_subqueries.push(query.clone());
    let schema = RowSchema::new(vec![]);
    let row = PhysicalRow::from_values(vec![]);
    let outer = PhysicalOuterRow::Physical {
        schema: &schema,
        row: &row,
    };
    for (expression, expected_event) in [
        (
            ScalarExpr::Func {
                name: "key_value".into(),
                args: vec![],
                distinct: false,
                filter: None,
                order_by: vec![],
                binding: None,
            },
            "key",
        ),
        (ScalarExpr::ScalarSubquery(0), "nested"),
    ] {
        cache_keys(&scope, vec![expression]);
        assert!(services
            .context(&scope)
            .subquery_exists(0, &query, outer, &[])
            .unwrap());
        assert_eq!(*services.events.lock(), [expected_event]);
        services.events.lock().clear();
    }
}

#[test]
fn null_exists_key_stops_before_evaluating_later_subqueries() {
    let services = Services::default();
    let scope = CteScope::new();
    let query = plan("SELECT 1");
    cache_keys(
        &scope,
        vec![
            ScalarExpr::Literal(Value::Null),
            ScalarExpr::ScalarSubquery(99),
        ],
    );
    let schema = RowSchema::new(vec![]);
    let row = PhysicalRow::from_values(vec![]);
    assert!(!services
        .context(&scope)
        .subquery_exists(
            0,
            &query,
            PhysicalOuterRow::Physical {
                schema: &schema,
                row: &row
            },
            &[]
        )
        .unwrap());
    let error = services
        .context(&scope)
        .subquery_exists(0, &query, PhysicalOuterRow::Absent, &[])
        .unwrap_err();
    assert!(
        matches!(error, SQLError::Internal(message) if message == "correlated subquery requires an outer row")
    );
    assert!(services.events.lock().is_empty());
}

#[test]
fn predicate_preparation_checks_shape_slot_and_volatility_before_catalog_capture() {
    let services = Services::default();
    let mut scope = CteScope::new();
    assert!(prepare_correlated_exists_predicate(
        &services.services(),
        &ScalarExpr::Literal(Value::Bool(true)),
        &[],
        &scope
    )
    .unwrap()
    .is_none());
    let expression = ScalarExpr::Exists {
        subquery: 0,
        negated: false,
    };
    let error = prepare_correlated_exists_predicate(&services.services(), &expression, &[], &scope)
        .err()
        .unwrap();
    assert!(
        matches!(error, SQLError::Internal(message) if message == "physical scalar subquery slot 0 is out of bounds")
    );
    assert!(services.events.lock().is_empty());
    scope
        .scalar_subqueries
        .push(plan("SELECT volatile_value()"));
    assert!(
        prepare_correlated_exists_predicate(&services.services(), &expression, &[], &scope)
            .unwrap()
            .is_none()
    );
    assert_eq!(*services.events.lock(), ["volatility"]);
}
