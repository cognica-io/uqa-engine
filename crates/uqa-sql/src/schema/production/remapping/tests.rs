//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::InternalRelationId,
    schema::join_output::{compile_layout_with_control, JoinOutputSource},
};
use uqa_core::{memory::MemoryBudget, CancellationToken};

fn fixture() -> RowSchema {
    let score = InternalRelationId::allocate().column(0);
    let input = RowSchema::from_physical_layout(crate::schema::PhysicalLayout {
        columns: ["a", "b", "a", "b", "rec"].map(String::from).into(),
        identities: [("l", "a"), ("l", "b"), ("r", "a"), ("r", "b"), ("r", "rec")]
            .map(|(qualifier, name)| ColumnIdentity::qualified(qualifier, name))
            .into(),
        types: [
            ColumnType::Integer,
            ColumnType::Text,
            ColumnType::BigInteger,
            ColumnType::Text,
            ColumnType::Record,
        ]
        .map(Some)
        .into(),
        slots: (0..5).map(Some).collect(),
        physical_width: 6,
        aliases: vec![(
            ColumnIdentity::qualified("l", "hidden"),
            Some(1),
            Some(ColumnType::Text),
        )],
        internal: vec![(score, Some(5), Some(ColumnType::DoublePrecision))],
        score_sources: vec![(Some("l".into()), score)],
        wildcard_hidden: [3].into(),
    })
    .unwrap()
    .with_column_collations([None, Some(950), None, Some(951), None])
    .with_record_fields([(
        4,
        vec![Some(ColumnType::Integer), Some(ColumnType::Text)].into(),
    )]);
    let input = RowSchema::with_open_columns(&input, Some("l"));
    input
        .with_nullable_relations(InternalRelationId::allocate().column(0))
        .unwrap()
}

#[test]
fn join_output_layout_retains_original_metadata_and_deduplicates_computed_positions() {
    let input = fixture();
    let merged = JoinOutputSource::Coalesce {
        left: 0,
        right: 2,
        ty: ColumnType::BigInteger,
    };
    let columns = vec![
        ("a".into(), ColumnIdentity::unqualified("a"), merged.clone()),
        (
            "b".into(),
            ColumnIdentity::qualified("l", "b"),
            JoinOutputSource::Input(1),
        ),
        (
            "rec".into(),
            ColumnIdentity::qualified("r", "rec"),
            JoinOutputSource::Input(4),
        ),
    ];
    let aliases = vec![
        (
            ColumnIdentity::qualified("l", "a"),
            JoinOutputSource::Input(0),
        ),
        (
            ColumnIdentity::qualified("r", "a"),
            JoinOutputSource::Input(2),
        ),
        (ColumnIdentity::qualified("merged", "a"), merged),
    ];
    let width = input.physical_width();
    let expected = expected_output(&input);
    let memory = MemoryBudget::new(4 << 20);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&memory, &original, &invoking);
    let (schema, computed) =
        compile_layout_with_control(&input, &columns, &aliases, &control).unwrap();
    assert_eq!(schema, expected);
    assert_eq!(computed.len(), 1);
    assert_eq!(schema.physical_width(), width + 1);
    assert_eq!(schema.column_collation(1), Some(950));
    assert!(std::ptr::eq(
        &raw const **schema.record_fields(2).unwrap(),
        &raw const **input.record_fields(4).unwrap()
    ));
    let retained = schema.clone();
    let held = memory.used();
    assert!(held > computed.reserved_bytes());
    drop(schema);
    assert_eq!(memory.used(), held);
    drop(computed);
    assert!(memory.used() > 0);
    drop(retained);
    assert_eq!(memory.used(), 0);
    for token in [&original, &invoking] {
        token.cancel();
        assert_eq!(
            compile_layout_with_control(&input, &columns, &aliases, &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(memory.used(), 0);
        token.reset();
    }
}

#[test]
fn physical_join_alias_replaces_namespaces_and_preserves_hidden_values_and_scores() {
    let input = fixture();
    let names = ["id", "dup", "right_id", "dup", "record"].map(String::from);
    let columns = names
        .iter()
        .enumerate()
        .map(|(position, name)| {
            (
                name.clone(),
                ColumnIdentity::qualified("joined", name),
                position,
                input.column_type(position).cloned(),
            )
        })
        .collect::<Vec<_>>();
    let expected = RowSchema::with_rebound_score_sources(
        &RowSchema::remap_typed_identities_without_input_aliases(&input, &columns),
        Some("joined"),
    );
    let memory = MemoryBudget::new(4 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let actual = crate::semantics::alias_join_physical_schema_with_control(
        &input, "joined", &names, &control,
    )
    .unwrap();
    assert_eq!(actual, expected);
    assert!(!actual.has_qualifier("l"));
    assert_eq!(actual.whole_row_presences().count(), 0);
    assert_eq!(actual.score_sources().next().unwrap().0, Some("joined"));
    drop(actual);
    assert_eq!(memory.used(), 0);
    let empty = RowSchema::new(vec![]);
    let output =
        crate::semantics::alias_join_physical_schema_with_control(&empty, "j", &[], &control)
            .unwrap();
    assert_eq!(output, RowSchema::with_relation_qualifier(&empty, "j"));
    drop(output);
    assert_eq!(memory.used(), 0);
    assert_eq!(
        crate::semantics::alias_join_physical_schema_with_control(
            &empty,
            "j",
            &["extra".into()],
            &control
        )
        .unwrap_err()
        .sqlstate(),
        Some("42P10")
    );
    assert_eq!(memory.used(), 0);
}

#[test]
fn join_output_rejects_wide_types_and_alias_names_before_allocation() {
    let memory = MemoryBudget::new(32 << 10);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let input = RowSchema::with_types(vec!["value".into()], vec![Some(ColumnType::Integer)]);
    let columns = vec![(
        "value".into(),
        ColumnIdentity::unqualified("value"),
        JoinOutputSource::Cast {
            input: 0,
            ty: ColumnType::Array(Box::new(ColumnType::Named("x".repeat(1 << 20)))),
        },
    )];
    let measured = allocation_counter::measure(|| {
        assert_eq!(
            compile_layout_with_control(&input, &columns, &[], &control)
                .unwrap_err()
                .sqlstate(),
            Some("53200")
        );
    });
    assert!(measured.bytes_total < 128 << 10, "{measured:?}");
    assert_eq!(memory.used(), 0);
    let alias = "a".repeat(1 << 20);
    let measured = allocation_counter::measure(|| {
        assert_eq!(
            crate::semantics::alias_join_physical_schema_with_control(
                &input,
                &alias,
                &[],
                &control
            )
            .unwrap_err()
            .sqlstate(),
            Some("53200")
        );
    });
    assert!(measured.bytes_total < 128 << 10, "{measured:?}");
    assert_eq!(memory.used(), 0);
    for (source, expected) in [
        (
            JoinOutputSource::Input(1),
            "join output input position 1 is outside width 1",
        ),
        (
            JoinOutputSource::Cast {
                input: 2,
                ty: ColumnType::Integer,
            },
            "join output cast position 2 is outside width 1",
        ),
        (
            JoinOutputSource::Coalesce {
                left: 0,
                right: 3,
                ty: ColumnType::Integer,
            },
            "join output coalesce positions (0, 3) are outside width 1",
        ),
    ] {
        let error = compile_layout_with_control(
            &input,
            &[("value".into(), ColumnIdentity::unqualified("value"), source)],
            &[],
            &control,
        )
        .unwrap_err();
        assert!(matches!(error, SQLError::Internal(ref message) if message == expected));
        assert_eq!(memory.used(), 0);
    }
}

fn expected_output(input: &RowSchema) -> RowSchema {
    let width = input.physical_width();
    let intermediate = RowSchema::append_hidden_typed(input, &[Some(ColumnType::BigInteger)]);
    RowSchema::remap_typed_physical_identities(
        &intermediate,
        &[
            (
                "a".into(),
                ColumnIdentity::unqualified("a"),
                width,
                Some(ColumnType::BigInteger),
            ),
            (
                "b".into(),
                ColumnIdentity::qualified("l", "b"),
                1,
                Some(ColumnType::Text),
            ),
            (
                "rec".into(),
                ColumnIdentity::qualified("r", "rec"),
                4,
                Some(ColumnType::Record),
            ),
        ],
        &[
            (
                ColumnIdentity::qualified("l", "a"),
                0,
                Some(ColumnType::Integer),
            ),
            (
                ColumnIdentity::qualified("r", "a"),
                2,
                Some(ColumnType::BigInteger),
            ),
            (
                ColumnIdentity::qualified("merged", "a"),
                width,
                Some(ColumnType::BigInteger),
            ),
        ],
    )
}
