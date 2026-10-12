//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn json_path() -> BuiltinFunctionOverload {
    BuiltinFunctionOverload {
        variadic: true,
        name: "pg_catalog.jsonb_extract_path".into(),
        argument_names: vec![Some("from_json".into()), Some("path_elems".into())],
        argument_types: vec![
            ColumnType::JsonB,
            ColumnType::Array(Box::new(ColumnType::Text)),
        ],
        default_arguments: 0,
        return_type: ColumnType::JsonB,
    }
}

#[test]
fn typed_variadic_calls_preserve_formal_identity_and_effective_argument_targets() {
    use crate::ast::RoutineVariadicMode;
    let builtins = [json_path()];
    let names = [None, None, None];
    let types = [
        Some(ColumnType::JsonB),
        None,
        Some(ColumnType::Varchar(Some(12))),
    ];
    let budget = MemoryBudget::new(16_384);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    let call = RoutineCallDescriptor {
        argument_names: &names,
        argument_types: &types,
        explicit_variadic: false,
    };
    let selected = resolve_local_builtin_call_with_control(
        "jsonb_extract_path",
        None,
        call,
        &builtins,
        &control,
    )
    .unwrap();
    assert_eq!(selected.binding.argument_types, ["jsonb", "text[]"]);
    let invocation = selected.binding.invocation.as_ref().unwrap();
    assert_eq!(invocation.argument_positions, [0, 1, 1]);
    assert_eq!(invocation.argument_targets, ["jsonb", "text", "text"]);
    assert_eq!(invocation.parameter_types, ["jsonb", "text[]"]);
    assert_eq!(
        invocation.variadic_mode,
        RoutineVariadicMode::Expanded { parameter_index: 1 }
    );
    let rebound = resolve_local_builtin_call_with_control(
        "jsonb_extract_path",
        Some(&selected.binding),
        call,
        &builtins,
        &control,
    )
    .unwrap();
    assert_eq!(*selected, *rebound);
    assert!(budget.used() > 0);
    drop((selected, rebound));
    assert_eq!(budget.used(), 0);

    let names = [Some("path_elems".into()), Some("from_json".into())];
    let types = [
        Some(ColumnType::Array(Box::new(ColumnType::Text))),
        Some(ColumnType::JsonB),
    ];
    let call = RoutineCallDescriptor {
        argument_names: &names,
        argument_types: &types,
        explicit_variadic: true,
    };
    let selected = resolve_local_builtin_call_with_control(
        "jsonb_extract_path",
        None,
        call,
        &builtins,
        &control,
    )
    .unwrap();
    let invocation = selected.binding.invocation.as_ref().unwrap();
    assert_eq!(invocation.argument_positions, [1, 0]);
    assert_eq!(invocation.argument_targets, ["text[]", "jsonb"]);
    assert_eq!(
        invocation.variadic_mode,
        RoutineVariadicMode::Explicit { parameter_index: 1 }
    );
    drop(selected);
    assert_eq!(budget.used(), 0);
}

#[test]
fn typed_variadic_matching_rejects_missing_wrong_and_implicitly_named_paths() {
    let array = ColumnType::Array(Box::new(ColumnType::Text));
    for (types, names, explicit) in [
        (vec![Some(ColumnType::JsonB)], vec![None], false),
        (
            vec![Some(ColumnType::JsonB), Some(array)],
            vec![None, None],
            false,
        ),
        (
            vec![Some(ColumnType::JsonB), Some(ColumnType::Boolean)],
            vec![None, None],
            false,
        ),
        (
            vec![Some(ColumnType::Json), Some(ColumnType::Text)],
            vec![None, None],
            false,
        ),
        (
            vec![
                Some(ColumnType::JsonB),
                Some(ColumnType::Array(Box::new(ColumnType::Integer))),
            ],
            vec![None, None],
            true,
        ),
        (
            vec![None, None],
            vec![Some("from_json".into()), Some("path_elems".into())],
            false,
        ),
    ] {
        assert!(match_builtin_function_call(
            json_path(),
            RoutineCallDescriptor {
                argument_names: &names,
                argument_types: &types,
                explicit_variadic: explicit,
            }
        )
        .is_none());
    }
}

#[test]
fn typed_variadic_matching_and_invocation_release_quota_and_cancellation_failures() {
    let builtins = [json_path()];
    let names = [None, None, None];
    let types = [None, None, None];
    let call = RoutineCallDescriptor {
        argument_names: &names,
        argument_types: &types,
        explicit_variadic: false,
    };
    for limit in [0, 16, 64, 128, 256, 512, 1024, 4096] {
        let budget = MemoryBudget::new(limit);
        let cancellation = CancellationToken::new();
        let control = ProductionControl::new(&budget, &cancellation, &cancellation);
        match resolve_local_builtin_call_with_control(
            "jsonb_extract_path",
            None,
            call,
            &builtins,
            &control,
        ) {
            Ok(value) => drop(value),
            Err(error) => assert_eq!(error.sqlstate(), Some("53200")),
        }
        assert_eq!(budget.used(), 0);
    }
    for cancel_query in [false, true] {
        let budget = MemoryBudget::new(4096);
        let query = CancellationToken::new();
        let work = CancellationToken::new();
        if cancel_query {
            query.cancel();
        } else {
            work.cancel();
        }
        let control = ProductionControl::new(&budget, &query, &work);
        let error = resolve_local_builtin_call_with_control(
            "jsonb_extract_path",
            None,
            call,
            &builtins,
            &control,
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(budget.used(), 0);
    }
}

fn overload(argument: ColumnType, result: ColumnType) -> BuiltinFunctionOverload {
    BuiltinFunctionOverload {
        variadic: false,
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
        collation_oid: None,
        schema: "public".into(),
        name: "rejected".repeat(1 << 15),
        oid: 90_001,
        array_oid: None,
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
