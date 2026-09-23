//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn parameter(name: &str, ty: &str, has_default: bool) -> FunctionParameterDescriptor {
    FunctionParameterDescriptor {
        name: Some(name.into()),
        type_name: ty.into(),
        has_default,
    }
}

#[test]
fn controlled_signature_preserves_named_defaults_and_coercion_scores() {
    let parameters = [
        parameter("optional", "integer", true),
        parameter("required", "bigint", false),
    ];
    let budget = MemoryBudget::new(8192);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let names: [Option<String>; 1] = [None];
    let types = [Some(ColumnType::Integer)];
    let actual = match_signature_with_control(parameters.as_slice(), &names, &types, &control)
        .unwrap()
        .unwrap();
    assert_eq!(actual.argument_positions, vec![1]);
    assert_eq!(actual.argument_types, vec!["int8"]);
    assert_eq!(actual.raw_exact_matches, 0);
    assert_eq!(
        *actual,
        match_function_signature(&parameters, &names, &types).unwrap()
    );
    assert_eq!(
        actual.reserved_bytes(),
        actual.argument_positions.capacity() * size_of::<usize>()
            + actual.argument_types.capacity() * size_of::<String>()
            + actual
                .argument_types
                .iter()
                .map(String::capacity)
                .sum::<usize>()
    );
    assert_eq!(budget.used(), actual.reserved_bytes());
    drop(actual);
    assert_eq!(budget.used(), 0);
    let names = [Some("required"), Some("optional")];
    let types = [Some(ColumnType::BigInteger), Some(ColumnType::Integer)];
    let named_match = match_signature_with_control(parameters.as_slice(), &names, &types, &control)
        .unwrap()
        .unwrap();
    assert_eq!(named_match.argument_positions, vec![1, 0]);
    assert_eq!(named_match.exact_matches, 2);
    drop(named_match);
    assert_eq!(budget.used(), 0);
}

#[test]
fn controlled_signature_keeps_semantic_mismatch_separate_from_resource_failure() {
    let parameters = [parameter("a", "text", false)];
    let token = CancellationToken::new();
    let budget = MemoryBudget::new(8192);
    let control = ProductionControl::new(&budget, &token, &token);
    let names: [Option<&str>; 1] = [None];
    assert!(match_signature_with_control(
        parameters.as_slice(),
        &names,
        &[Some(ColumnType::Boolean)],
        &control
    )
    .unwrap()
    .is_none());
    assert_eq!(budget.used(), 0);
    let budget = MemoryBudget::new(size_of::<usize>() * 2);
    let control = ProductionControl::new(&budget, &token, &token);
    assert!(match_signature_with_control(
        parameters.as_slice(),
        &names,
        &[Some(ColumnType::Text)],
        &control
    )
    .is_err());
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
        assert!(matches!(
            match_signature_with_control(
                parameters.as_slice(),
                &names,
                &[Some(ColumnType::Text)],
                &control
            ),
            Err(ValueRetentionError::Cancelled(_))
        ));
        assert_eq!(budget.used(), 0);
    }
}
