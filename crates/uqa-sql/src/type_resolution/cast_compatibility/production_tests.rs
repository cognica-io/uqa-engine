//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn controlled_cast_compatibility_preserves_catalog_array_and_text_rules() {
    let budget = MemoryBudget::new(64 * 1024);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    for (source, target, expected) in [
        (ColumnType::Integer, ColumnType::BigInteger, true),
        (ColumnType::Varchar(Some(12)), ColumnType::Text, true),
        (
            ColumnType::Array(Box::new(ColumnType::Integer)),
            ColumnType::Array(Box::new(ColumnType::Text)),
            true,
        ),
        (
            ColumnType::Int2Vector,
            ColumnType::Array(Box::new(ColumnType::BigInteger)),
            true,
        ),
        (ColumnType::Date, ColumnType::Integer, false),
    ] {
        assert_eq!(
            explicit_type_compatible_with_control(&source, &target, &control).unwrap(),
            expected
        );
        assert_eq!(budget.used(), 0);
    }
    let error = validate_explicit_cast_with_control(
        Some(&ColumnType::Date),
        &ColumnType::Integer,
        &control,
    )
    .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42846"));
    assert_eq!(budget.used(), 0);
}

#[test]
fn identical_inline_types_and_unknown_sources_need_no_scratch() {
    let budget = MemoryBudget::new(0);
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    assert!(explicit_type_compatible_with_control(
        &ColumnType::Integer,
        &ColumnType::Integer,
        &control
    )
    .unwrap());
    validate_explicit_cast_with_control(
        None,
        &ColumnType::Array(Box::new(ColumnType::Text)),
        &control,
    )
    .unwrap();
    assert_eq!(budget.peak(), 0);
}

#[test]
fn cast_compatibility_propagates_quota_and_cancellation_without_false_rejection() {
    let array = ColumnType::Array(Box::new(ColumnType::Integer));
    let budget = MemoryBudget::new(size_of::<ColumnType>());
    let cancellation = CancellationToken::new();
    let control = ProductionControl::new(&budget, &cancellation, &cancellation);
    assert_eq!(
        validate_explicit_cast_with_control(Some(&array), &array, &control)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(budget.used(), 0);
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
            validate_explicit_cast_with_control(None, &ColumnType::Integer, &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), 0);
    }
}
