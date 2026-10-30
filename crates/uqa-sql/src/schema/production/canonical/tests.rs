//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{ast::InternalRelationId, ColumnType};
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn fixture() -> RowSchema {
    let relation = InternalRelationId::allocate();
    let mut metadata = SchemaBuildMetadata::default();
    metadata.aliases.extend([
        (ColumnIdentity::qualified("z", "hidden"), 8),
        (ColumnIdentity::qualified("a", "hidden"), 3),
        (ColumnIdentity::unqualified("absent"), NULL_SLOT),
    ]);
    metadata
        .internal
        .extend([(relation.column(0), 8), (relation.column(1), 3)]);
    metadata.internal_types.extend([
        (relation.column(0), Some(ColumnType::Text)),
        (relation.column(1), Some(ColumnType::Integer)),
    ]);
    metadata.wildcard_hidden.insert(3);
    RowSchema::from_typed_parts_with_aliases_and_exact_precedence(
        vec!["b".into(), "a".into(), "gone".into(), "b".into()],
        ["b", "a", "gone", "b"]
            .map(|column| ColumnIdentity::qualified("q", column))
            .into(),
        vec![
            Some(ColumnType::Text),
            Some(ColumnType::Integer),
            None,
            Some(ColumnType::Text),
        ],
        vec![7, 3, NULL_SLOT, 7],
        9,
        metadata,
    )
}

#[test]
fn canonical_capture_preserves_hidden_alias_order_null_slots_and_internal_types() {
    let original = fixture();
    let (expected, positions) = original.canonical_projection();
    assert_eq!(positions, [7, 3, 8]);
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut output = None;
    let measured = allocation_counter::measure(|| {
        output = Some(
            original
                .canonical_projection_with_control(&control)
                .unwrap(),
        );
    });
    let (schema, slots) = output.unwrap();
    assert_eq!(schema, expected);
    assert_eq!(*slots, positions);
    // The schema and its thirteen admitted lookup indexes exclude their shared reference counters from the payload allowance.
    assert_eq!(
        measured.bytes_current,
        budget.used() as i64 + (14 * 2 * size_of::<usize>()) as i64
    );
    let retained = budget.used();
    let shared = schema.clone();
    drop(schema);
    assert_eq!(budget.used(), retained);
    drop((slots, shared));
    assert_eq!(budget.used(), 0);
}

#[test]
fn canonical_capture_rejects_before_wide_name_copies_and_releases_partial_layouts() {
    let original = RowSchema::new(vec!["first".into(), "x".repeat(4 << 20)]);
    for limit in [0, 128, 1024, 8192] {
        let parent = MemoryBudget::new(limit + 31);
        let sibling = parent.reserve(31).unwrap();
        let budget = parent.child(limit);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let measured = allocation_counter::measure(|| {
            assert_eq!(
                original
                    .canonical_projection_with_control(&control)
                    .unwrap_err()
                    .sqlstate(),
                Some("53200")
            );
        });
        assert!(measured.bytes_total < 64 << 10, "{measured:?}");
        assert_eq!(budget.used(), 0);
        assert_eq!(parent.used(), sibling.bytes());
    }
    for request in [false, true] {
        let budget = MemoryBudget::new(32 << 20);
        let original_token = CancellationToken::new();
        let session = CancellationToken::new();
        if request {
            original_token.cancel();
        } else {
            session.cancel();
        }
        let control = ProductionControl::new(&budget, &original_token, &session);
        assert_eq!(
            original
                .canonical_projection_with_control(&control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), 0);
    }
    let budget = MemoryBudget::new(32 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let value = original
        .canonical_projection_with_control(&control)
        .unwrap();
    assert!(budget.used() > 8 << 20);
    drop(value);
    assert_eq!(budget.used(), 0);
}
