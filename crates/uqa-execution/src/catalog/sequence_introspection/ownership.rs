//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered sequence owner enumeration under actual registry read guards.
use super::SequenceIntrospectionCatalog;
use uqa_storage::StorageBackendResult;
pub fn sequence_names_owned_by_tables(
    sequences: &dyn SequenceIntrospectionCatalog,
    table_object_ids: &std::collections::BTreeSet<[u8; 16]>,
) -> StorageBackendResult<std::collections::BTreeSet<String>> {
    sequences.refresh_sequences()?;
    Ok(sequences
        .states()
        .iter()
        .filter(|(_, state)| {
            state
                .owner
                .is_some_and(|owner| table_object_ids.contains(&owner.table_object_id))
        })
        .map(|(relation, _)| relation.qualified_name())
        .collect())
}
pub fn sequence_names_owned_by_column(
    sequences: &dyn SequenceIntrospectionCatalog,
    table_object_id: [u8; 16],
    column_object_id: [u8; 16],
) -> StorageBackendResult<std::collections::BTreeSet<String>> {
    sequences.refresh_sequences()?;
    Ok(sequences
        .states()
        .iter()
        .filter(|(_, state)| {
            state.owner.is_some_and(|owner| {
                owner.table_object_id == table_object_id
                    && owner.column_object_id == column_object_id
            })
        })
        .map(|(relation, _)| relation.qualified_name())
        .collect())
}
