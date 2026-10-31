//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ColumnType;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn null_rejection_join_aliases_admit_names_and_preserve_side_ownership() {
    let left =
        RowSchema::with_qualified_types("l", vec!["id".into()], vec![Some(ColumnType::Integer)]);
    let right =
        RowSchema::with_qualified_types("r", vec!["id".into()], vec![Some(ColumnType::Integer)]);
    let using = super::super::resolve_join_using(None, true, &left, &right)
        .unwrap()
        .unwrap();
    let budget = MemoryBudget::new(32 << 20);
    let query = CancellationToken::new();
    let session = CancellationToken::new();
    let control = ProductionControl::new(&budget, &query, &session);
    let wide = "x".repeat(4 << 20);
    let occupied = budget.reserve(budget.available() - (64 << 10)).unwrap();
    let mut rejected = None;
    let allocation = allocation_counter::measure(|| {
        rejected = Some(join_alias_input_schemas_with_control(
            JoinKind::Left,
            &left,
            &right,
            Some(&using),
            &wide,
            &[],
            &control,
        ));
    });
    assert!(allocation.bytes_total < 128 << 10, "{allocation:?}");
    assert_eq!(rejected.unwrap().unwrap_err().sqlstate(), Some("53200"));
    assert_eq!(budget.used(), occupied.bytes());
    drop(occupied);
    for (kind, left_visible, right_visible) in [
        (JoinKind::Inner, true, false),
        (JoinKind::Left, true, false),
        (JoinKind::Right, false, true),
        (JoinKind::Full, false, false),
    ] {
        let (a, b) = join_alias_input_schemas_with_control(
            kind,
            &left,
            &right,
            Some(&using),
            "joined",
            &[],
            &control,
        )
        .unwrap();
        assert_eq!(a.has_qualified_column("joined", "id"), left_visible);
        assert_eq!(b.has_qualified_column("joined", "id"), right_visible);
        assert!(a.has_qualified_column("l", "id"));
        assert!(b.has_qualified_column("r", "id"));
        assert!(budget.used() > 0);
        drop((a, b));
        assert_eq!(budget.used(), 0);
    }
    let (a, b) = join_alias_input_schemas_with_control(
        JoinKind::Inner,
        &left,
        &right,
        None,
        "joined",
        &[],
        &control,
    )
    .unwrap();
    assert!(!a.has_qualified_column("joined", "id"));
    assert!(!b.has_qualified_column("joined", "id"));
    drop((a, b));
    for token in [&query, &session] {
        token.cancel();
        assert_eq!(
            join_alias_input_schemas_with_control(
                JoinKind::Left,
                &left,
                &right,
                Some(&using),
                "joined",
                &[],
                &control
            )
            .unwrap_err()
            .sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), 0);
        token.reset();
    }
}
