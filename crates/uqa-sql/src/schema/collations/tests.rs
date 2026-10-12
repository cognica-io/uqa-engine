//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected collation identity through SQL row layout transformations.

use super::*;
use crate::ast::InternalRelationId;
use crate::schema::{PhysicalLayout, ProjectedSlot};

fn text(names: &[&str], collations: &[u32]) -> RowSchema {
    RowSchema::with_types(
        names.iter().map(|name| (*name).into()).collect(),
        vec![Some(ColumnType::Text); names.len()],
    )
    .with_column_collations(collations.iter().copied().map(Some))
}

#[test]
fn selected_collations_follow_alias_projection_join_and_canonical_layout() {
    let internal = InternalRelationId::allocate().column(0);
    let hidden = ColumnIdentity::qualified("hidden", "source");
    let source = text(&["c", "p", "conflict"], &[950, 951, 0]);
    let source = RowSchema::with_identity_aliases(&source, &[(hidden.clone(), 1)]);
    let source = RowSchema::with_physical_internal_aliases(
        &source,
        &[(internal, 0, Some(ColumnType::Text))],
    );
    assert_eq!(source.qualified_collation("hidden", "source"), Some(951));
    assert_eq!(source.internal_collation(internal), Some(950));
    let joined = RowSchema::join(&source, &text(&["extra"], &[811]), []);
    assert_eq!(joined.column_collation(3), Some(811));
    let projected = RowSchema::select(
        &joined,
        &[
            ("second".into(), "p".into()),
            ("first".into(), "c".into()),
            ("last".into(), "conflict".into()),
        ],
    );
    assert_eq!(
        (0..3)
            .map(|position| projected.column_collation(position))
            .collect::<Vec<_>>(),
        [Some(951), Some(950), Some(0)]
    );
    assert_eq!(projected.qualified_collation("hidden", "source"), None);
    assert_eq!(projected.internal_collation(internal), Some(950));
    let (canonical, _) = projected.canonical_projection();
    for position in 0..3 {
        assert_eq!(
            canonical.column_collation(position),
            projected.column_collation(position)
        );
    }
    assert_eq!(canonical.internal_collation(internal), Some(950));
    let qualified = RowSchema::with_relation_qualifier(&canonical, "out");
    assert_eq!(qualified.qualified_collation("out", "second"), Some(951));
    assert_eq!(qualified.qualified_collation("out", "last"), Some(0));
    let replaced =
        RowSchema::append_typed(&qualified, &[("second".into(), Some(ColumnType::Text))]);
    assert_eq!(replaced.collation_of("second"), Some(100));
    assert_eq!(replaced.collation_of("first"), Some(950));
    let projected = RowSchema::project_with_sources(
        &source,
        vec![
            (
                "c".into(),
                Some(ColumnType::Text),
                ProjectedSlot::Input(Some(1)),
            ),
            (
                "conflict".into(),
                Some(ColumnType::Text),
                ProjectedSlot::Computed(0),
            ),
        ],
        vec![],
        1,
        true,
    );
    assert_eq!(projected.collation_of("c"), Some(951));
    assert_eq!(projected.collation_of("conflict"), Some(100));
}

#[test]
fn null_slots_and_correlated_scopes_keep_distinct_logical_collations() {
    let source = RowSchema::from_physical_layout(PhysicalLayout {
        columns: vec!["c".into(), "p".into()],
        identities: vec![
            ColumnIdentity::qualified("outer", "c"),
            ColumnIdentity::qualified("outer", "p"),
        ],
        types: vec![Some(ColumnType::Text); 2],
        slots: vec![None; 2],
        physical_width: 0,
        aliases: vec![],
        internal: vec![],
        score_sources: vec![],
        wildcard_hidden: std::collections::HashSet::default(),
    })
    .unwrap()
    .with_column_collations([Some(950), Some(951)]);
    let hidden = ColumnIdentity::qualified("empty", "value");
    let source = RowSchema::with_identity_aliases(&source, &[(hidden, 1)]);
    assert_eq!(source.qualified_collation("empty", "value"), Some(951));
    let swapped = RowSchema::remap_positions(&source, &[("p".into(), 1), ("c".into(), 0)], &[]);
    assert_eq!(swapped.column_collation(0), Some(951));
    assert_eq!(swapped.column_collation(1), Some(950));
    let (canonical, slots) = swapped.canonical_projection();
    assert!(slots.is_empty());
    assert_eq!(canonical.collation_of("p"), Some(951));
    assert_eq!(canonical.collation_of("c"), Some(950));
    let inner = text(&["c"], &[100]);
    let scope = RowSchema::with_outer_schema(&inner, &source);
    assert_eq!(scope.collation_of("c"), Some(100));
    assert_eq!(scope.collation_of("p"), Some(951));
    assert_eq!(scope.qualified_collation("outer", "c"), Some(950));
    assert_eq!(scope.qualified_collation("empty", "value"), Some(951));
    let ambiguous = RowSchema::join(
        &source,
        &RowSchema::with_relation_qualifier(&text(&["c"], &[811]), "other"),
        [],
    );
    assert_eq!(ambiguous.collation_of("c"), None);
    assert_eq!(ambiguous.qualified_collation("outer", "c"), Some(950));
    assert_eq!(ambiguous.qualified_collation("other", "c"), Some(811));
}

#[test]
fn type_defaults_need_no_collation_payload_and_replacement_discards_old_selection() {
    let plain = text(&["a", "b"], &[100, 100]);
    assert!(plain.index.cold.collations.0.is_none());
    assert!(RowSchema::join(&plain, &RowSchema::default(), [])
        .index
        .cold
        .collations
        .0
        .is_none());
    let selected = plain.with_column_collations([Some(950), Some(951)]);
    let restored = selected.with_column_collations([None, None]);
    assert!(restored.index.cold.collations.0.is_none());
    let source = RowSchema::with_types(
        vec!["text".into(), "number".into()],
        vec![Some(ColumnType::Text), Some(ColumnType::Integer)],
    )
    .with_column_collations([Some(951), Some(0)]);
    let remapped = RowSchema::remap_typed_positions(
        &source,
        &[
            ("name".into(), 0, Some(ColumnType::Name)),
            ("integer".into(), 0, Some(ColumnType::Integer)),
            ("string".into(), 1, Some(ColumnType::Text)),
        ],
        &[],
    );
    assert_eq!(remapped.collation_of("name"), Some(951));
    assert_eq!(remapped.collation_of("integer"), Some(0));
    assert_eq!(remapped.collation_of("string"), Some(100));
}

#[test]
fn binding_only_collations_survive_outer_scopes_without_becoming_physical() {
    let identity = ColumnIdentity::qualified("generated", "value");
    let source = RowSchema::with_typed_virtual_identities(
        &RowSchema::default(),
        &[(identity.clone(), Some(ColumnType::Text))],
    )
    .with_virtual_collation(&identity, 951)
    .unwrap();
    let outer = RowSchema::with_outer_schema(&RowSchema::default(), &source);
    assert_eq!(outer.qualified_collation("generated", "value"), Some(951));
    assert_eq!(outer.physical_width(), 0);
    assert_eq!(outer.collation_overrides().count(), 0);
    let inner = RowSchema::with_typed_virtual_identities(
        &RowSchema::default(),
        &[(identity.clone(), Some(ColumnType::Text))],
    )
    .with_virtual_collation(&identity, 950)
    .unwrap();
    let scoped = RowSchema::with_outer_schema(&inner, &source);
    assert_eq!(scoped.qualified_collation("generated", "value"), Some(950));
}

#[test]
fn scalar_projection_uses_the_bound_collation_for_shared_and_null_slots() {
    for physical in [Some(0), None] {
        let schema = RowSchema::from_physical_layout(PhysicalLayout {
            columns: vec!["c".into(), "p".into()],
            identities: vec![
                ColumnIdentity::unqualified("c"),
                ColumnIdentity::unqualified("p"),
            ],
            types: vec![Some(ColumnType::Text); 2],
            slots: vec![physical; 2],
            physical_width: usize::from(physical.is_some()),
            aliases: vec![],
            internal: vec![],
            score_sources: vec![],
            wildcard_hidden: std::collections::HashSet::default(),
        })
        .unwrap()
        .with_column_collations([Some(950), Some(951)]);
        let projected = RowSchema::project_with_sources(
            &schema,
            vec![
                (
                    "p".into(),
                    Some(ColumnType::Text),
                    ProjectedSlot::BoundInput {
                        physical,
                        collation: schema.column_collation(1),
                    },
                ),
                (
                    "c".into(),
                    Some(ColumnType::Text),
                    ProjectedSlot::BoundInput {
                        physical,
                        collation: schema.column_collation(0),
                    },
                ),
            ],
            vec![],
            0,
            false,
        );
        assert_eq!(projected.column_collation(0), Some(951));
        assert_eq!(projected.column_collation(1), Some(950));
        assert_eq!(projected.physical_width(), schema.physical_width());
    }
}

#[test]
fn a_right_alias_default_replaces_a_left_selected_collation() {
    let identity = ColumnIdentity::qualified("alias", "v");
    let left = RowSchema::with_identity_aliases(&text(&["a"], &[100]), &[(identity.clone(), 0)])
        .with_collation_override(CollationBinding::Alias(&identity), 951)
        .unwrap();
    let right = RowSchema::with_identity_aliases(&text(&["b"], &[100]), &[(identity, 0)]);
    let joined = RowSchema::join(&left, &right, []);
    assert_eq!(joined.qualified_collation("alias", "v"), Some(100));
    assert!(joined.index.cold.collations.0.is_none());
}
