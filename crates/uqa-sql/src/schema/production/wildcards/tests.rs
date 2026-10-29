//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::schema::PhysicalLayout;
use std::collections::HashSet;
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn schema() -> RowSchema {
    RowSchema::from_physical_layout(PhysicalLayout {
        columns: vec!["later".into(), "first".into(), "absent".into()],
        identities: ["later", "first", "absent"]
            .map(|column| ColumnIdentity::qualified("r", column))
            .into(),
        types: vec![Some(ColumnType::Integer); 3],
        slots: vec![Some(2), Some(0), None],
        physical_width: 3,
        aliases: vec![
            (
                ColumnIdentity::qualified("r", "first"),
                Some(0),
                Some(ColumnType::Integer),
            ),
            (
                ColumnIdentity::qualified("r", "hidden"),
                Some(1),
                Some(ColumnType::Text),
            ),
        ],
        internal: vec![],
        score_sources: vec![],
        wildcard_hidden: HashSet::new(),
    })
    .unwrap()
}

#[test]
fn borrowed_wildcard_positions_match_visible_hidden_and_null_layout() {
    let schema = schema();
    let expected = schema.qualified_star_position_layout("r");
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut actual = Vec::new();
    schema
        .visit_qualified_star_positions("r", &control, |name, logical, slot, ty| {
            actual.push((name.to_string(), logical, slot, ty.cloned()));
            Ok(())
        })
        .unwrap();
    assert_eq!(actual, expected);
    assert_eq!(budget.used(), 0);
}

#[test]
fn wildcard_failure_and_cancellation_release_sort_scratch() {
    let schema = schema();
    let budget = MemoryBudget::new(1 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let sibling = budget.reserve(29).unwrap();
    let mut visited = 0;
    let error = schema
        .visit_qualified_star_positions("r", &control, |_, _, _, _| {
            visited += 1;
            invoking.cancel();
            Ok(())
        })
        .unwrap_err();
    assert_eq!(visited, 1);
    assert_eq!(error.sqlstate(), Some("57014"));
    assert_eq!(budget.used(), 29);
    invoking.reset();
    let error = schema
        .visit_qualified_star_positions("r", &control, |_, _, _, _| {
            Err(SQLError::Internal("consumer failed".into()))
        })
        .unwrap_err();
    assert!(matches!(error, SQLError::Internal(message) if message == "consumer failed"));
    assert_eq!(budget.used(), 29);
    let empty = budget.child(0);
    let empty_control = ProductionControl::new(&empty, &original, &invoking);
    assert_eq!(
        schema
            .visit_qualified_star_positions("r", &empty_control, |_, _, _, _| panic!(
                "unadmitted visitor"
            ))
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    drop(sibling);
    assert_eq!(budget.used(), 0);
}
