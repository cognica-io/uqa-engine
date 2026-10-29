//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{fixture::Catalog, integer};
use crate::{
    plan::OwnedScalarExpr, routines::inlining::selection::InlineSelection, ColumnType, ScalarExpr,
};
use std::sync::atomic::Ordering;
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken, Value,
};

#[test]
fn owned_selection_preserves_strict_null_and_immutable_execution_precedence() {
    let catalog = Catalog::new(
        "CREATE FUNCTION strict_value(x integer) RETURNS text LANGUAGE sql STRICT AS $$SELECT 'value'$$;
         CREATE FUNCTION constant_value(x integer) RETURNS integer LANGUAGE sql IMMUTABLE AS $$SELECT x+1$$",
    );
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for (name, argument, expected, ty, evaluations) in [
        (
            "strict_value",
            ScalarExpr::Literal(Value::Null),
            Value::Null,
            ColumnType::Text,
            0,
        ),
        (
            "constant_value",
            integer(7),
            Value::Int(37),
            ColumnType::Integer,
            1,
        ),
    ] {
        let binding = catalog.binding(name, &[Some(ColumnType::Integer)]);
        let mut selected = None;
        let allocations = allocation_counter::measure(|| {
            selected = Some(
                catalog
                    .context()
                    .select_inline_with_control(
                        &binding,
                        std::slice::from_ref(&argument),
                        &[],
                        &control,
                    )
                    .unwrap(),
            );
        });
        let InlineSelection::Expression(replacement) = selected.unwrap() else {
            panic!("selected constant replacement");
        };
        assert!(replacement.error_context.is_none());
        assert_eq!(replacement.identity, binding.object_id.unwrap());
        assert!(
            matches!(&*replacement.expression, ScalarExpr::TypedLiteral {value, bound_type: Some(actual), ..} if value == &expected && actual == &ty)
        );
        assert_eq!(
            allocations.bytes_current,
            i64::try_from(budget.used()).unwrap()
        );
        assert_eq!(replacement.expression.reserved_bytes(), budget.used());
        assert_eq!(catalog.evaluations.load(Ordering::SeqCst), evaluations);
        assert!(catalog.take_notices().is_empty());
        drop(replacement);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn owned_body_selection_substitutes_argument_owners_and_retires_preparation_scratch() {
    let catalog =
        Catalog::new("CREATE FUNCTION echo(x text) RETURNS text LANGUAGE sql AS $$SELECT x$$");
    let binding = catalog.binding("echo", &[Some(ColumnType::Text)]);
    let argument = ScalarExpr::TypedLiteral {
        value: Value::Str("retained caller text".repeat(1024)),
        ty: "text".into(),
        bound_type: Some(ColumnType::Text),
        collation_oid: None,
        composite_source: None,
        parameter_index: None,
    };
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut result = None;
    let allocations = allocation_counter::measure(|| {
        let InlineSelection::Body(candidate) = catalog
            .context()
            .select_inline_with_control(&binding, std::slice::from_ref(&argument), &[], &control)
            .unwrap()
        else {
            panic!("body analysis candidate");
        };
        let expression = ScalarExpr::Param(1).copy_for_rewrite(&control).unwrap();
        result = candidate
            .substitute(expression, &[], &control, &catalog.context())
            .unwrap();
    });
    let replacement = result.unwrap();
    assert_eq!(*replacement.expression, argument);
    let context = replacement.error_context.as_ref().unwrap();
    assert_eq!(context.as_str(), "SQL function \"echo\" during inlining");
    assert_eq!(
        budget.used(),
        replacement.expression.reserved_bytes() + context.reserved_bytes()
    );
    assert_eq!(
        allocations.bytes_current,
        i64::try_from(budget.used()).unwrap()
    );
    assert_eq!(catalog.evaluations.load(Ordering::SeqCst), 0);
    drop(replacement);
    assert_eq!(budget.used(), 0);
}

#[test]
fn owned_selection_rejects_argument_payloads_and_both_cancellations_before_body_work() {
    let catalog =
        Catalog::new("CREATE FUNCTION echo(x text) RETURNS text LANGUAGE sql AS $$SELECT x$$");
    let binding = catalog.binding("echo", &[Some(ColumnType::Text)]);
    let argument = ScalarExpr::Literal(Value::Str("x".repeat(1 << 20)));
    let budget = MemoryBudget::new(4096);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let allocations = allocation_counter::measure(|| {
        let error = catalog
            .context()
            .select_inline_with_control(&binding, std::slice::from_ref(&argument), &[], &control)
            .err()
            .expect("quota rejection");
        assert_eq!(error.sqlstate(), Some("53200"));
    });
    assert!(allocations.bytes_max < 8192, "{allocations:?}");
    assert_eq!(allocations.bytes_current, 0);
    assert_eq!(budget.used(), 0);
    for original_cancelled in [true, false] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if original_cancelled {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let control = ProductionControl::new(&budget, &original, &invoking);
        assert!(control.check().is_err());
        let allocations = allocation_counter::measure(|| {
            let error = catalog
                .context()
                .select_inline_with_control(
                    &binding,
                    std::slice::from_ref(&argument),
                    &[],
                    &control,
                )
                .err()
                .expect("cancelled selection");
            assert_eq!(error.sqlstate(), Some("57014"));
        });
        assert_eq!(allocations.count_total, 0);
        assert_eq!(budget.used(), 0);
    }
    assert!(catalog.take_notices().is_empty());
    assert_eq!(catalog.evaluations.load(Ordering::SeqCst), 0);
}

#[test]
fn declined_owned_body_destroys_every_argument_and_diagnostic_owner() {
    let catalog = Catalog::new("CREATE FUNCTION strict_value(x integer) RETURNS integer LANGUAGE sql STRICT AS $$SELECT 1$$");
    let binding = catalog.binding("strict_value", &[Some(ColumnType::Integer)]);
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let InlineSelection::Body(candidate) = catalog
        .context()
        .select_inline_with_control(&binding, &[integer(7)], &[], &control)
        .unwrap()
    else {
        panic!("body candidate");
    };
    let expression = OwnedScalarExpr::literal(
        control
            .finish(Value::Int(1), control.empty_reservation())
            .unwrap(),
        &control,
    )
    .unwrap();
    assert!(candidate
        .substitute(expression, &[], &control, &catalog.context())
        .unwrap()
        .is_none());
    assert_eq!(budget.used(), 0);
}
