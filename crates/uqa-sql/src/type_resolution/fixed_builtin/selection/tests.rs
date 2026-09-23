//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::type_resolution::{fixed_builtin::overloads, resolve_local_builtin_overload};
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn borrowed_fixed_declarations_preserve_legacy_candidate_selection() {
    let budget = MemoryBudget::new(256 * 1024);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    for (name, names, types) in [
        ("abs", vec![None], vec![Some(ColumnType::Integer)]),
        ("PG_CATALOG.abs", vec![None], vec![Some(ColumnType::Real)]),
        ("reverse", vec![None], vec![None]),
        (
            "jsonb_strip_nulls",
            vec![None],
            vec![Some(ColumnType::JsonB)],
        ),
        (
            "jsonb_strip_nulls",
            vec![Some("strip_in_arrays".into()), Some("target".into())],
            vec![Some(ColumnType::Boolean), Some(ColumnType::JsonB)],
        ),
    ] {
        let declared = overloads(name).unwrap();
        let expected =
            resolve_local_builtin_overload(name, None, &names, &types, &declared).unwrap();
        let selected =
            resolve_overload_with_control(name, None, &names, &types, false, &control).unwrap();
        assert_eq!(*selected, expected);
        assert_eq!(budget.used(), selected.reserved_bytes());
        let bound = resolve_overload_with_control(
            name,
            Some(&selected.binding),
            &names,
            &types,
            false,
            &control,
        )
        .unwrap();
        assert_eq!(*bound, expected);
        drop(bound);
        drop(selected);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn fixed_selector_keeps_bound_and_unbound_diagnostics() {
    let budget = MemoryBudget::new(256 * 1024);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    for (name, names, types) in [
        ("abs", vec![None], vec![Some(ColumnType::Boolean)]),
        (
            "abs",
            vec![Some("missing".into())],
            vec![Some(ColumnType::Integer)],
        ),
        ("round", vec![None], vec![None]),
    ] {
        let declared = overloads(name).unwrap();
        let expected = resolve_local_builtin_overload(name, None, &names, &types, &declared);
        let result = resolve_overload_with_control(name, None, &names, &types, false, &control);
        match (expected, result) {
            (Ok(expected), Ok(actual)) => assert_eq!(expected, *actual),
            (Err(expected), Err(actual)) => {
                assert_eq!(actual.sqlstate(), expected.sqlstate());
                assert_eq!(actual.to_string(), expected.to_string());
            }
            (expected, actual) => panic!("candidate outcomes differ: {expected:?} / {actual:?}"),
        }
        assert_eq!(budget.used(), 0);
    }
    let declared = overloads("abs").unwrap();
    let mut binding = resolve_local_builtin_overload(
        "abs",
        None,
        &[None],
        &[Some(ColumnType::Integer)],
        &declared,
    )
    .unwrap()
    .binding;
    binding.argument_types[0] = "text".into();
    let expected = resolve_local_builtin_overload(
        "abs",
        Some(&binding),
        &[None],
        &[Some(ColumnType::Integer)],
        &declared,
    )
    .unwrap_err();
    let actual = resolve_overload_with_control(
        "abs",
        Some(&binding),
        &[None],
        &[Some(ColumnType::Integer)],
        false,
        &control,
    )
    .unwrap_err();
    assert_eq!(actual.to_string(), expected.to_string());
    assert_eq!(budget.used(), 0);
}

#[test]
fn fixed_matching_and_output_reservations_release_on_resource_failure() {
    let cancellation = CancellationToken::new();
    for limit in [0, 32, 128, 1024] {
        let budget = MemoryBudget::new(limit);
        let control = ProductionControl::new(&budget, &cancellation, &cancellation);
        let result = resolve_overload_with_control("abs", None, &[None], &[None], false, &control);
        if let Err(error) = &result {
            assert!(matches!(error.sqlstate(), Some("53200" | "42725")));
        }
        drop(result);
        assert_eq!(budget.used(), 0);
        assert!(budget.peak() <= limit);
    }
    let budget = MemoryBudget::new(4096);
    for cancel_original in [false, true] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel_original {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let control = ProductionControl::new(&budget, &original, &invoking);
        assert_eq!(
            resolve_overload_with_control(
                "abs",
                None,
                &[None],
                &[Some(ColumnType::Integer)],
                false,
                &control
            )
            .unwrap_err()
            .sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), 0);
    }
}
