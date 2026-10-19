//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn native_pattern_identity_preserves_shared_expression_storage_bounds() {
    assert!(size_of::<crate::ast::FunctionDispatch>() <= 12);
    #[cfg(target_pointer_width = "64")]
    {
        assert!(size_of::<crate::ast::FunctionBinding>() <= 104);
        assert!(size_of::<crate::ScalarExpr>() <= 192);
    }
}

#[test]
fn pattern_overloads_match_independent_postgresql_catalog() {
    let rows: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("tests/overloads.json")).unwrap();
    assert_eq!(rows.len(), 27);
    let memory = MemoryBudget::new(16 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    for row in rows {
        let operator = PatternOperator::from_symbol(row["name"].as_str().unwrap()).unwrap();
        let left = ColumnType::from_sql_name(row["left_type"].as_str().unwrap()).unwrap();
        let right = ColumnType::from_sql_name(row["right_type"].as_str().unwrap()).unwrap();
        let selected = select(operator, &[Some(&left), Some(&right)], &control).unwrap();
        assert_eq!(selected.identity.oid, row["oid"].as_i64().unwrap());
        assert_eq!(
            selected.identity.function_oid,
            row["function_oid"].as_i64().unwrap()
        );
        drop(selected);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn prefix_operator_selects_text_and_rejects_unrelated_operands() {
    let memory = MemoryBudget::new(16 * 1024);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    for input in [
        None,
        Some(ColumnType::Text),
        Some(ColumnType::Varchar(None)),
    ] {
        let selected = select(
            PatternOperator::StartsWith,
            &[input.as_ref(), None],
            &control,
        )
        .unwrap();
        assert_eq!(selected.argument(0), &ColumnType::Text);
        assert_eq!(selected.argument(1), &ColumnType::Text);
        assert_eq!(selected.identity.oid, 3877);
        drop(selected);
        assert_eq!(memory.used(), 0);
    }
    for ty in [ColumnType::Integer, ColumnType::Boolean, ColumnType::Bytea] {
        let error = select(
            PatternOperator::StartsWith,
            &[Some(&ty), Some(&ty)],
            &control,
        )
        .err()
        .unwrap();
        assert_eq!(error.sqlstate(), Some("42883"));
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn pattern_selection_releases_workspace_on_signature_quota_and_cancellation_errors() {
    let memory = MemoryBudget::new(16 * 1024);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&memory, &original, &invoking);
    let error = select(
        PatternOperator::Like,
        &[Some(&ColumnType::Integer), None],
        &control,
    )
    .err()
    .unwrap();
    assert_eq!(error.sqlstate(), Some("42883"));
    assert_eq!(
        error.to_string(),
        "operator does not exist: integer ~~ unknown"
    );
    assert_eq!(memory.used(), 0);
    let error = select_escape(false, &[None, Some(&ColumnType::Integer)], &control).unwrap_err();
    assert_eq!(error.sqlstate(), Some("42883"));
    assert_eq!(
        error.to_string(),
        "function pg_catalog.like_escape(unknown, integer) does not exist"
    );
    assert_eq!(memory.used(), 0);
    let small = MemoryBudget::new(1);
    let constrained = ProductionControl::new(&small, &original, &invoking);
    assert_eq!(
        select(PatternOperator::Like, &[None, None], &constrained)
            .err()
            .unwrap()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(small.used(), 0);
    invoking.cancel();
    assert_eq!(
        select(PatternOperator::Like, &[None, None], &control)
            .err()
            .unwrap()
            .sqlstate(),
        Some("57014")
    );
    assert_eq!(memory.used(), 0);
}
