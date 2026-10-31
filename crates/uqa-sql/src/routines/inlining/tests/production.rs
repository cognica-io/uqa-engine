//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::fixture::Catalog;
use crate::{ColumnType, ScalarExpr};
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken, Value,
};

#[test]
fn body_parameter_values_and_context_frames_retain_exact_destination_storage() {
    let catalog = Catalog::new(
        "CREATE FUNCTION typed(a integer, b text) RETURNS integer LANGUAGE sql AS $$SELECT a$$",
    );
    let definition = &catalog.functions[0].def;
    let budget = MemoryBudget::new(1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut params = None;
    let allocated = allocation_counter::measure(|| {
        params = Some(
            crate::routines::body_validation::routine_parameter_values_with_control(
                &catalog, definition, &control,
            )
            .unwrap(),
        );
    });
    let params = params.unwrap();
    assert_eq!(params[0].declared_scalar_type(), Some(&ColumnType::Integer));
    assert_eq!(params[1].declared_scalar_type(), Some(&ColumnType::Text));
    assert_eq!(allocated.bytes_current as usize, params.reserved_bytes());
    let mut context = None;
    let allocated = allocation_counter::measure(|| {
        context = Some(
            crate::routines::diagnostics::sql_context_with_control(
                definition,
                crate::routines::diagnostics::SQLRoutineStage::Inlining,
                &control,
            )
            .unwrap(),
        );
    });
    let context = context.unwrap();
    assert_eq!(context.as_str(), "SQL function \"typed\" during inlining");
    assert_eq!(allocated.bytes_current as usize, context.reserved_bytes());
    assert_eq!(
        budget.used(),
        params.reserved_bytes() + context.reserved_bytes()
    );
    drop((params, context));
    assert_eq!(budget.used(), 0);
}

#[test]
fn owned_parameter_substitution_keeps_caller_parameters_and_exact_copied_literals() {
    let expression = ScalarExpr::Array(vec![
        ScalarExpr::Param(1),
        ScalarExpr::Param(2),
        ScalarExpr::Param(1),
    ]);
    let arguments = [
        ScalarExpr::Param(7),
        ScalarExpr::Literal(Value::Str("literal".repeat(4096))),
    ];
    let budget = MemoryBudget::new(1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let uses = super::super::owned_substitution::use_counts_with_control(
        &expression,
        arguments.len(),
        &control,
    )
    .unwrap();
    assert_eq!(&*uses, &[2, 1]);
    drop(uses);
    let mut output = None;
    let allocation = allocation_counter::measure(|| {
        output = Some(
            super::super::owned_substitution::substitute_with(
                expression.copy_for_rewrite(&control).unwrap(),
                |index| &arguments[index],
                &control,
            )
            .unwrap(),
        );
    });
    let output = output.unwrap();
    let ScalarExpr::Array(items) = &*output else {
        panic!("array expression");
    };
    assert_eq!(
        items,
        &[
            arguments[0].clone(),
            arguments[1].clone(),
            arguments[0].clone()
        ]
    );
    assert_eq!(allocation.bytes_current as usize, output.reserved_bytes());
    assert_eq!(budget.used(), output.reserved_bytes());
    drop(output);
    assert_eq!(budget.used(), 0);
}
