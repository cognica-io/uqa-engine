//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn overload(argument: ColumnType, result: ColumnType) -> BuiltinFunctionOverload {
    BuiltinFunctionOverload {
        name: "pg_catalog.f".into(),
        argument_names: vec![Some("value".into())],
        argument_types: vec![argument],
        default_arguments: 0,
        return_type: result,
    }
}

#[test]
fn local_resolution_borrows_rejected_declarations_and_retains_only_selected_result() {
    let huge_result = ColumnType::Domain {
        schema: "public".into(),
        name: "rejected".repeat(1 << 15),
        oid: 90_001,
        base: Box::new(ColumnType::Text),
    };
    let builtins = [
        overload(ColumnType::Boolean, huge_result),
        overload(ColumnType::Integer, ColumnType::Text),
    ];
    let budget = MemoryBudget::new(4096);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    let selected = resolve_local_builtin_overload_with_control(
        "f",
        None,
        &[None],
        &[Some(ColumnType::Integer)],
        &builtins,
        &control,
    )
    .unwrap();
    assert_eq!(selected.binding.name, "pg_catalog.f");
    assert_eq!(selected.binding.argument_types, ["integer"]);
    assert_eq!(selected.return_type, ColumnType::Text);
    assert_eq!(selected.exact_matches, 1);
    let retained = selected.binding.name.capacity()
        + selected.binding.argument_types.capacity() * size_of::<String>()
        + selected
            .binding
            .argument_types
            .iter()
            .map(String::capacity)
            .sum::<usize>();
    assert_eq!(selected.reserved_bytes(), retained);
    assert_eq!(budget.used(), retained);
    drop(selected);
    assert_eq!(budget.used(), 0);
}

#[test]
fn borrowed_signature_keeps_named_defaults_binding_identity_and_semantic_errors() {
    let mut defaulted = overload(ColumnType::Integer, ColumnType::Text);
    defaulted.argument_names.push(Some("fallback".into()));
    defaulted.argument_types.push(ColumnType::Text);
    defaulted.default_arguments = 1;
    let builtins = [defaulted];
    let budget = MemoryBudget::new(8192);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    let names = [Some("value".into())];
    let types = [Some(ColumnType::Integer)];
    let ordinary = resolve_local_builtin_overload("f", None, &names, &types, &builtins).unwrap();
    for binding in [None, Some(&ordinary.binding)] {
        let selected = resolve_local_builtin_overload_with_control(
            "f", binding, &names, &types, &builtins, &control,
        )
        .unwrap();
        assert_eq!(*selected, ordinary);
        drop(selected);
        assert_eq!(budget.used(), 0);
    }
    let unknown_name = [Some("absent".into())];
    let error = resolve_local_builtin_overload_with_control(
        "f",
        None,
        &unknown_name,
        &types,
        &builtins,
        &control,
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42883"));
    assert_eq!(budget.used(), 0);
    let ambiguous = [
        overload(ColumnType::Boolean, ColumnType::Boolean),
        overload(ColumnType::Integer, ColumnType::Integer),
    ];
    let error = resolve_local_builtin_overload_with_control(
        "f",
        None,
        &[None],
        &[None],
        &ambiguous,
        &control,
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42725"));
    assert_eq!(budget.used(), 0);
}

#[test]
fn overload_resource_failures_release_partial_candidates_without_semantic_fallback() {
    let builtins = [
        overload(ColumnType::SmallInteger, ColumnType::SmallInteger),
        overload(ColumnType::Integer, ColumnType::Integer),
        overload(ColumnType::BigInteger, ColumnType::BigInteger),
    ];
    for limit in [0, 16, 64, 128, 256, 512, 1024, 4096] {
        let budget = MemoryBudget::new(limit);
        let cancellation = CancellationToken::new();
        let control = ProductionControl::new(&budget, &cancellation, &cancellation);
        match resolve_local_builtin_overload_with_control(
            "f",
            None,
            &[None],
            &[Some(ColumnType::SmallInteger)],
            &builtins,
            &control,
        ) {
            Ok(selected) => {
                assert_eq!(selected.return_type, ColumnType::SmallInteger);
                drop(selected);
            }
            Err(error) => assert_eq!(error.sqlstate(), Some("53200")),
        }
        assert_eq!(budget.used(), 0);
    }
    for cancel_original in [true, false] {
        let budget = MemoryBudget::new(4096);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let error = resolve_local_builtin_overload_with_control(
            "f",
            None,
            &[None],
            &[Some(ColumnType::Integer)],
            &builtins,
            &ProductionControl::new(&budget, &original, &invoking),
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn malformed_parameter_metadata_does_not_enter_the_shared_matcher() {
    let mut builtin = overload(ColumnType::Integer, ColumnType::Integer);
    builtin.default_arguments = 2;
    assert!(match_builtin_function_overload(builtin.clone(), &[None], &[None]).is_none());
    builtin.default_arguments = 0;
    builtin.argument_names.clear();
    assert!(match_builtin_function_overload(builtin, &[None], &[None]).is_none());
    assert!(builtin_name_matches("F", "PG_CATALOG.f"));
    assert!(!builtin_name_matches("other.F", "PG_CATALOG.f"));
}
