//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::MemoryBudget, CancellationToken};

#[test]
fn physical_projection_schema_preserves_slots_duplicates_types_and_internal_metadata() {
    let input = RowSchema::with_qualified_types(
        "source",
        vec!["id".into(), "name".into()],
        vec![Some(ColumnType::Integer), Some(ColumnType::Text)],
    );
    let internal = crate::ast::InternalRelationId::allocate().column(0);
    let projected = vec![
        (
            "name".to_string(),
            Some(ColumnType::Text),
            ProjectedSlot::Input(Some(1)),
        ),
        (
            "name".to_string(),
            Some(ColumnType::Integer),
            ProjectedSlot::Computed {
                position: 0,
                collation: None,
            },
        ),
        ("missing".to_string(), None, ProjectedSlot::Input(None)),
    ];
    let hidden = vec![(
        internal,
        Some(ColumnType::Integer),
        ProjectedSlot::Input(Some(0)),
    )];
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    for append in [false, true] {
        let expected =
            RowSchema::project_with_sources(&input, projected.clone(), hidden.clone(), 1, append);
        let actual = RowSchema::project_with_sources_with_control(
            &input,
            projected
                .iter()
                .map(|(name, ty, source)| (name.as_str(), ty.as_ref(), *source)),
            &[(internal, hidden[0].1.as_ref(), hidden[0].2)],
            1,
            append,
            &control,
        )
        .unwrap();
        assert_eq!(actual.columns(), expected.columns());
        assert_eq!(actual.identities(), expected.identities());
        assert_eq!(actual.column_types(), expected.column_types());
        assert_eq!(actual.physical_width(), expected.physical_width());
        assert_eq!(
            actual.internal_slot(internal),
            expected.internal_slot(internal)
        );
        assert_eq!(
            actual.qualified_slot("source", "name"),
            expected.qualified_slot("source", "name")
        );
        for position in 0..actual.len() {
            assert_eq!(
                actual.physical_slot(position),
                expected.physical_slot(position)
            );
            assert_eq!(
                actual.column_collation(position),
                expected.column_collation(position)
            );
        }
        let retained = actual.clone();
        drop(actual);
        assert!(budget.used() > 0);
        drop(retained);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn physical_schema_admission_precedes_large_label_copy_and_observes_both_cancellations() {
    let input = RowSchema::new(Vec::new());
    let label = "x".repeat(4 << 20);
    let budget = MemoryBudget::new(64 << 10);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let project = || {
        RowSchema::project_with_sources_with_control(
            &input,
            [(
                label.as_str(),
                Some(&ColumnType::Text),
                ProjectedSlot::Computed {
                    position: 0,
                    collation: None,
                },
            )],
            &[],
            1,
            false,
            &control,
        )
    };
    let mut error = None;
    let allocation = allocation_counter::measure(|| {
        error = Some(project().unwrap_err());
    });
    assert!(allocation.bytes_total < 256 << 10, "{allocation:?}");
    assert_eq!(error.unwrap().sqlstate(), Some("53200"));
    assert_eq!(budget.used(), 0);
    for token in [&original, &invoking] {
        token.cancel();
        assert_eq!(project().unwrap_err().sqlstate(), Some("57014"));
        assert_eq!(budget.used(), 0);
        token.reset();
    }
}
