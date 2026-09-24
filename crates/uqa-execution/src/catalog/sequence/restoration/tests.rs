//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

mod authority;
use uqa_storage::{SequenceOwner, SequenceOwnerDependency};

fn row() -> SequenceRow {
    let state = SequenceState {
        start: 17,
        increment: 3,
        current: 20,
        called: true,
        log_count: 4,
        data_type: SequenceDataType::BigInt,
        min_value: 1,
        max_value: i64::MAX,
        cycle: false,
        cache_size: 7,
        definition_generation: [3; 16],
        owner: Some(SequenceOwner {
            table_object_id: [4; 16],
            column_object_id: [5; 16],
            dependency: SequenceOwnerDependency::Internal,
        }),
    };
    sequence_row(
        "public.ids",
        [2; 16],
        state,
        RelationPersistence::Permanent,
        &BoundSequenceSecurity::owner(uqa_core::catalog_role::RoleIdentity::BOOTSTRAP),
    )
    .unwrap()
}

#[test]
fn sequence_values_follow_commits_without_losing_private_name_and_definition_changes() {
    let named = |name: &str, object: u8, value: i64| {
        let mut row = row();
        row.relation.name = name.into();
        row.object_id = [object; 16];
        row.current = value;
        row
    };
    let bound = vec![
        named("values", 1, 20),
        named("renamed", 2, 40),
        named("created", 4, 50),
        named("removed_by_peer", 5, 60),
    ];
    let mut committed_value = named("values", 1, 300);
    committed_value.definition_generation = [8; 16];
    let selected = select_sequence_value_rows(
        bound,
        vec![
            committed_value,
            named("old_name", 2, 10),
            named("privately_dropped", 3, 15),
            named("created_by_peer", 6, 70),
        ],
        |row| Ok(matches!(row.object_id[0], 2..=4)),
    )
    .unwrap();
    assert_eq!(
        selected
            .iter()
            .map(|row| (row.relation.name.as_str(), row.current))
            .collect::<Vec<_>>(),
        [
            ("created", 50),
            ("created_by_peer", 70),
            ("renamed", 40),
            ("values", 300)
        ]
    );
    assert_eq!(selected.last().unwrap().definition_generation, [8; 16]);
}

#[test]
fn failed_private_sequence_inspection_does_not_return_a_partial_registry() {
    let error = select_sequence_value_rows(Vec::new(), vec![row()], |_| {
        Err(StorageBackendError::Other(
            "private view unavailable".into(),
        ))
    })
    .unwrap_err();
    assert_eq!(error.to_string(), "private view unavailable");
}

#[test]
fn durable_sequence_row_round_trip_preserves_all_allocation_and_owner_fields() {
    let original = row();
    let (relation, state) = sequence_state_from_row(original.clone()).unwrap();
    let restored = sequence_row(
        &relation.qualified_name(),
        original.object_id,
        state,
        RelationPersistence::Permanent,
        &security::restore_security(
            &original.security,
            &BTreeMap::from([(
                "uqa".into(),
                uqa_sql::catalog::roles::RoleDefinition::bootstrap(),
            )]),
            false,
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(restored).unwrap(),
        serde_json::to_value(original).unwrap()
    );
}
#[test]
fn omitted_durable_bounds_follow_integer_width_and_increment_direction() {
    for (name, data_type) in [
        ("smallint", SequenceDataType::SmallInt),
        ("integer", SequenceDataType::Integer),
        ("bigint", SequenceDataType::BigInt),
    ] {
        let (min, max) = data_type.bounds();
        for ascending in [false, true] {
            let mut input = row();
            input.options.data_type = name.into();
            input.increment = if ascending { 1 } else { -1 };
            input.start = input.increment;
            input.current = input.start;
            input.options.min_value = None;
            input.options.max_value = None;
            let (_, state) = sequence_state_from_row(input).unwrap();
            assert_eq!(state.data_type, data_type);
            assert_eq!(
                (state.min_value, state.max_value),
                if ascending { (1, max) } else { (min, -1) }
            );
        }
    }
}
#[test]
fn malformed_sequence_rows_keep_increment_log_count_and_type_error_precedence() {
    let mut input = row();
    input.increment = 0;
    input.log_count = -1;
    input.options.data_type = "unknown".into();
    assert_eq!(
        sequence_state_from_row(input.clone())
            .unwrap_err()
            .to_string(),
        "corrupt sequence `public.ids` has zero increment"
    );
    input.increment = 1;
    assert_eq!(
        sequence_state_from_row(input.clone())
            .unwrap_err()
            .to_string(),
        "corrupt sequence `public.ids` has a negative log count"
    );
    input.log_count = 0;
    assert_eq!(
        sequence_state_from_row(input).unwrap_err().to_string(),
        "corrupt sequence `public.ids` has data type `unknown`"
    );
}
#[test]
fn missing_generation_precedes_invalid_sequence_definition_without_repairing_the_input() {
    let mut input = row();
    input.definition_generation = [0; 16];
    input.options.cache_size = 0;
    let before = serde_json::to_value(&input).unwrap();
    assert_eq!(
        sequence_state_from_row(input.clone())
            .unwrap_err()
            .to_string(),
        "corrupt sequence `public.ids` has no definition generation"
    );
    assert_eq!(serde_json::to_value(&input).unwrap(), before);
    input.definition_generation = [9; 16];
    let error = sequence_state_from_row(input).unwrap_err();
    assert!(error
        .to_string()
        .starts_with("corrupt sequence `public.ids` definition: "));
}
