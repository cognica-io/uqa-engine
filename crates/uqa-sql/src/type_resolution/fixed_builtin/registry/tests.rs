//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};

#[test]
fn descriptors_keep_overloaded_aliases_named_defaults_and_signature_order() {
    assert_eq!(lookup("PG_CATALOG.ABS").unwrap().1.len(), 6);
    assert_eq!(lookup("generate_series").unwrap().1.len(), 9);
    assert_eq!(lookup("substr").unwrap().1.len(), 4);
    assert_eq!(lookup("substring").unwrap().1.len(), 6);
    assert_eq!(lookup("pow").unwrap().0, "pow");
    assert_eq!(lookup("power").unwrap().0, "power");
    assert!(lookup("public.abs").is_none());
    let (_, nulls) = lookup("jsonb_strip_nulls").unwrap();
    assert_eq!(nulls.len(), 1);
    assert_eq!(nulls[0].argument_names, &["target", "strip_in_arrays"]);
    assert_eq!(nulls[0].default_arguments, 1);
    assert_eq!(
        nulls[0].argument_types,
        &[ColumnType::JsonB, ColumnType::Boolean]
    );
    assert_eq!(nulls[0].return_type, ColumnType::JsonB);
    let (_, random) = lookup("random").unwrap();
    assert!(random[0].argument_types.is_empty());
    assert_eq!(random[1].argument_names, &["min", "max"]);
    assert_eq!(
        random[1].argument_types,
        &[ColumnType::Integer, ColumnType::Integer]
    );
}

fn binding(name: &str, argument: &str) -> crate::ast::FunctionBinding {
    crate::ast::FunctionBinding {
        object_id: None,
        name: name.into(),
        argument_types: vec![argument.into()],
        builtin: true,
        dispatch: None,
        invocation: None,
        resolution_error: None,
    }
}

#[test]
fn selected_bindings_borrow_one_descriptor_without_retaining_scratch() {
    let budget = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let selected = bound_signature(&binding("PG_CATALOG.ABS", "INT4"), &control)
        .unwrap()
        .unwrap();
    assert_eq!(selected.argument_types, &[ColumnType::Integer]);
    assert_eq!(selected.return_type, ColumnType::Integer);
    assert_eq!(budget.used(), 0);
    let repeated = bound_signature(&binding("pg_catalog.abs", "integer"), &control)
        .unwrap()
        .unwrap();
    assert!(std::ptr::eq(selected, repeated));
    for (name, argument) in [
        ("abs", "integer"),
        ("public.abs", "integer"),
        ("pg_catalog.abs", "text"),
        ("pg_catalog.not_real", "integer"),
    ] {
        assert!(bound_signature(&binding(name, argument), &control)
            .unwrap()
            .is_none());
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn selected_binding_resource_failures_never_become_missing_signatures() {
    let token = CancellationToken::new();
    let budget = MemoryBudget::new(0);
    let control = ProductionControl::new(&budget, &token, &token);
    assert_eq!(
        bound_signature(&binding("pg_catalog.abs", "integer"), &control)
            .err()
            .unwrap()
            .sqlstate(),
        Some("53200")
    );
    for cancel_original in [true, false] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let control = ProductionControl::new(&budget, &original, &invoking);
        assert_eq!(
            bound_signature(&binding("pg_catalog.abs", "integer"), &control)
                .err()
                .unwrap()
                .sqlstate(),
            Some("57014")
        );
    }
    assert_eq!(budget.used(), 0);
}
