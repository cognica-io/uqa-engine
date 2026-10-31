//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{call, fixture::Catalog, integer};
use crate::{ColumnType, ScalarExpr};
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken, Value,
};

#[test]
fn materialized_defaults_keep_exact_owners_and_match_ordinary_invocations() {
    let mut catalog = Catalog::new("CREATE FUNCTION defaults(a integer,b text DEFAULT 'tail') RETURNS text LANGUAGE sql AS $$SELECT b$$");
    let payload = "retained_default".repeat(2048);
    std::sync::Arc::make_mut(&mut catalog.functions[0])
        .def
        .params[1]
        .default = Some(crate::ast::Expr::Literal(Value::Str(payload.clone())));
    let binding = catalog.binding("defaults", &[Some(ColumnType::Integer)]);
    let input = call(binding.clone(), vec![integer(7)]);
    let expected = catalog
        .context()
        .materialize_call(&binding, &[integer(7)])
        .unwrap()
        .unwrap();
    let budget = MemoryBudget::new(4 * 1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut output = None;
    let allocations = allocation_counter::measure(|| {
        let input = input.copy_for_rewrite(&control).unwrap();
        output = Some(
            catalog
                .context()
                .materialize_owned_call(input, &control)
                .unwrap(),
        );
    });
    let output = output.unwrap();
    assert_eq!(allocations.bytes_current as usize, output.reserved_bytes());
    assert_eq!(budget.used(), output.reserved_bytes());
    let ScalarExpr::Func {
        binding: Some(actual),
        args,
        ..
    } = &*output
    else {
        panic!("selected function");
    };
    assert_eq!(actual, &expected.0);
    assert_eq!(args, &expected.1);
    let immutable = output.finish(&control).unwrap();
    assert_eq!(budget.used(), immutable.reserved_bytes());
    drop(immutable);
    assert_eq!(budget.used(), 0);
}

#[test]
fn variadic_materialization_preserves_marker_and_argument_order() {
    let catalog = Catalog::new("CREATE FUNCTION variadic_inputs(VARIADIC a integer[]) RETURNS integer LANGUAGE sql AS $$SELECT 1$$");
    let binding = catalog.binding(
        "variadic_inputs",
        &[Some(ColumnType::Integer), Some(ColumnType::Integer)],
    );
    let expected = catalog
        .context()
        .materialize_call(&binding, &[integer(4), integer(5)])
        .unwrap()
        .unwrap();
    let input = call(binding, vec![integer(4), integer(5)]);
    let budget = MemoryBudget::new(1024 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let output = catalog
        .context()
        .materialize_owned_call(input.copy_for_rewrite(&control).unwrap(), &control)
        .unwrap();
    let ScalarExpr::Func {
        binding: Some(actual),
        args,
        ..
    } = &*output
    else {
        panic!("selected function");
    };
    assert_eq!(actual, &expected.0);
    assert_eq!(args, &expected.1);
    assert!(
        matches!(&args[0], ScalarExpr::Func { binding: Some(marker), .. } if marker.dispatch == Some(crate::ast::FunctionDispatch::VariadicArgument))
    );
    drop(output);
    assert_eq!(budget.used(), 0);
}

#[test]
fn default_rejection_precedes_payload_allocation_and_retires_consumed_call() {
    let mut catalog = Catalog::new("CREATE FUNCTION large_default(a text DEFAULT 'tail') RETURNS text LANGUAGE sql AS $$SELECT a$$");
    std::sync::Arc::make_mut(&mut catalog.functions[0])
        .def
        .params[0]
        .default = Some(crate::ast::Expr::Literal(Value::Str("large".repeat(32768))));
    let input = call(catalog.binding("large_default", &[]), vec![]);
    let budget = MemoryBudget::new(16384);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let input = input.copy_for_rewrite(&control).unwrap();
    let allocations = allocation_counter::measure(|| {
        assert_eq!(
            catalog
                .context()
                .materialize_owned_call(input, &control)
                .unwrap_err()
                .sqlstate(),
            Some("53200")
        );
    });
    assert!(allocations.bytes_total < 16384, "{allocations:?}");
    assert_eq!(budget.used(), 0);
}

#[test]
fn malformed_invocation_and_both_cancellations_release_the_call_owner() {
    let catalog = Catalog::new(
        "CREATE FUNCTION input(a integer) RETURNS integer LANGUAGE sql AS $$SELECT a$$",
    );
    for failure in 0..3 {
        let mut binding = catalog.binding("input", &[Some(ColumnType::Integer)]);
        if failure == 0 {
            binding.invocation.as_mut().unwrap().parameter_types.clear();
        }
        let input = call(binding, vec![integer(1)]);
        let budget = MemoryBudget::new(1024 * 1024);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let input = input.copy_for_rewrite(&control).unwrap();
        if failure == 1 {
            original.cancel();
        }
        if failure == 2 {
            invoking.cancel();
        }
        let error = catalog
            .context()
            .materialize_owned_call(input, &control)
            .unwrap_err();
        assert_eq!(
            error.sqlstate(),
            Some(if failure == 0 { "XX000" } else { "57014" })
        );
        assert_eq!(budget.used(), 0);
    }
}
