//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Restore durable sequence metadata and migrate legacy rows inside the caller's open transaction.
use super::{sequence_row, SequenceState};
use crate::catalog::sequence_introspection::SequenceIntrospectionCatalog;
use std::{collections::BTreeMap, ops::Deref};
use uqa_core::RelationIdentity;
use uqa_sql::{
    ast::{RelationPersistence, SequenceDataType},
    catalog::security::{sequence_inquiry::SequenceSecurityCatalog, SequenceSecurity},
};
use uqa_storage::{CatalogFacade, SequenceRow, StorageBackendError, StorageBackendResult};
pub const SEQUENCES_METADATA_KEY: &str = "sql_sequences_json";
pub type SequencePersistenceRead<'a> =
    Box<dyn Deref<Target = BTreeMap<RelationIdentity, RelationPersistence>> + 'a>;
pub struct RestoredSequenceRegistry {
    pub sequences: BTreeMap<RelationIdentity, SequenceState>,
    pub object_ids: BTreeMap<RelationIdentity, [u8; 16]>,
    pub persistence: BTreeMap<RelationIdentity, RelationPersistence>,
    pub security: BTreeMap<RelationIdentity, SequenceSecurity>,
}
pub trait SequenceRestoreRegistry {
    fn persistence(&self) -> SequencePersistenceRead<'_>;
    fn install(&self, registry: RestoredSequenceRegistry);
}
pub struct SequenceRestoreContext<'a> {
    pub sequences: &'a dyn SequenceIntrospectionCatalog,
    pub security: &'a dyn SequenceSecurityCatalog,
    pub registry: &'a dyn SequenceRestoreRegistry,
}
/// Initial-open migration; the allocator must return a fresh nonzero object identity.
pub fn migrate_legacy_sequences_from_metadata(
    catalog: &dyn CatalogFacade,
    new_object_id: fn() -> StorageBackendResult<[u8; 16]>,
) -> StorageBackendResult<()> {
    // One-time, restart-safe migration from the former all-sequences JSON
    // snapshot. Merge idempotently even after a partially completed run,
    // then clear the legacy payload so deliberately dropping every typed
    // sequence cannot resurrect the old snapshot on the next open.
    if let Some(json) = catalog.get_metadata(SEQUENCES_METADATA_KEY)? {
        let legacy = serde_json::from_str::<BTreeMap<String, SequenceState>>(&json)?;
        if !legacy.is_empty() {
            for (name, state) in legacy {
                catalog.create_sequence_row(&sequence_row(
                    &name,
                    new_object_id()?,
                    state,
                    uqa_sql::ast::RelationPersistence::Permanent,
                    &SequenceSecurity {
                        role_owner: "uqa".into(),
                        acl: None,
                    },
                )?)?;
            }
            catalog.set_metadata(SEQUENCES_METADATA_KEY, "{}")?;
        }
    }
    Ok(())
}
/// Initial-open migration; the allocator must return a fresh nonzero object identity.
pub fn migrate_sequence_identities(
    catalog: &dyn CatalogFacade,
    new_object_id: fn() -> StorageBackendResult<[u8; 16]>,
) -> StorageBackendResult<()> {
    let mut identities = std::collections::BTreeSet::new();
    for mut row in catalog.load_sequence_rows()? {
        let mut changed = false;
        if row.object_id == [0; 16] || !identities.insert(row.object_id) {
            loop {
                let object_id = new_object_id()?;
                if identities.insert(object_id) {
                    row.object_id = object_id;
                    changed = true;
                    break;
                }
            }
        }
        if row.definition_generation == [0; 16] {
            row.definition_generation = row.object_id;
            changed = true;
        }
        if !changed {
            continue;
        }
        if !catalog.replace_sequence_row(&row)? {
            return Err(StorageBackendError::Other(format!(
                "sequence `{}` disappeared while assigning its object identity",
                row.relation.qualified_name()
            )));
        }
    }
    Ok(())
}
pub fn sequence_state_from_row(
    row: SequenceRow,
) -> StorageBackendResult<(RelationIdentity, SequenceState)> {
    if row.increment == 0 {
        return Err(StorageBackendError::Other(format!(
            "corrupt sequence `{}` has zero increment",
            row.relation.qualified_name()
        )));
    }
    if row.log_count < 0 {
        return Err(StorageBackendError::Other(format!(
            "corrupt sequence `{}` has a negative log count",
            row.relation.qualified_name()
        )));
    }
    let data_type = match row.options.data_type.as_str() {
        "smallint" => SequenceDataType::SmallInt,
        "integer" => SequenceDataType::Integer,
        "bigint" => SequenceDataType::BigInt,
        other => {
            return Err(StorageBackendError::Other(format!(
                "corrupt sequence `{}` has data type `{other}`",
                row.relation.qualified_name()
            )))
        }
    };
    let (type_min, type_max) = data_type.bounds();
    let state = SequenceState {
        start: row.start,
        increment: row.increment,
        current: row.current,
        called: row.called,
        log_count: row.log_count,
        data_type,
        min_value: row
            .options
            .min_value
            .unwrap_or(if row.increment > 0 { 1 } else { type_min }),
        max_value: row
            .options
            .max_value
            .unwrap_or(if row.increment > 0 { type_max } else { -1 }),
        cycle: row.options.cycle,
        cache_size: row.options.cache_size,
        definition_generation: row.definition_generation,
        owner: row.owner,
    };
    if state.definition_generation == [0; 16] {
        return Err(StorageBackendError::Other(format!(
            "corrupt sequence `{}` has no definition generation",
            row.relation.qualified_name()
        )));
    }
    uqa_sql::schema::sequences::definition::validate_sequence_definition(&state.definition(), None)
        .map_err(|error| {
            StorageBackendError::Other(format!(
                "corrupt sequence `{}` definition: {error}",
                row.relation.qualified_name()
            ))
        })?;
    Ok((row.relation, state))
}
pub fn restore_sequence_rows(
    context: &SequenceRestoreContext<'_>,
    rows: Vec<SequenceRow>,
) -> StorageBackendResult<()> {
    let temporary_persistence = context
        .registry
        .persistence()
        .iter()
        .filter(|(_, persistence)| **persistence == uqa_sql::ast::RelationPersistence::Temporary)
        .map(|(relation, persistence)| (relation.clone(), *persistence))
        .collect::<BTreeMap<_, _>>();
    let mut sequences = context
        .sequences
        .states()
        .iter()
        .filter(|(relation, _)| temporary_persistence.contains_key(*relation))
        .map(|(relation, state)| (relation.clone(), *state))
        .collect::<BTreeMap<_, _>>();
    let mut object_ids = context
        .sequences
        .object_ids()
        .iter()
        .filter(|(relation, _)| temporary_persistence.contains_key(*relation))
        .map(|(relation, object_id)| (relation.clone(), *object_id))
        .collect::<BTreeMap<_, _>>();
    let mut security = context
        .security
        .security_read()
        .iter()
        .filter(|(relation, _)| temporary_persistence.contains_key(*relation))
        .map(|(relation, security)| (relation.clone(), security.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut seen_object_ids = object_ids
        .values()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    let mut persistence = temporary_persistence;
    for row in rows {
        let name = row.relation.qualified_name();
        if row.role_owner.is_empty() {
            return Err(StorageBackendError::Other(format!(
                "corrupt sequence `{name}` has an empty role owner"
            )));
        }
        if row.object_id == [0; 16] {
            return Err(StorageBackendError::Other(format!(
                "corrupt sequence `{name}` has no object identity"
            )));
        }
        if !seen_object_ids.insert(row.object_id) {
            return Err(StorageBackendError::Other(format!(
                "corrupt sequence `{name}` has a duplicate object identity"
            )));
        }
        let object_id = row.object_id;
        let stored = match row.persistence.as_str() {
            "p" => uqa_sql::ast::RelationPersistence::Permanent,
            "u" => uqa_sql::ast::RelationPersistence::Unlogged,
            other => {
                return Err(StorageBackendError::Other(format!(
                    "corrupt sequence `{name}` persistence `{other}`"
                )))
            }
        };
        let role_owner = row.role_owner.clone();
        let acl = row.acl.clone();
        let (relation, state) = sequence_state_from_row(row)?;
        persistence.insert(relation.clone(), stored);
        object_ids.insert(relation.clone(), object_id);
        security.insert(relation.clone(), SequenceSecurity { role_owner, acl });
        sequences.insert(relation, state);
    }
    context.registry.install(RestoredSequenceRegistry {
        sequences,
        object_ids,
        persistence,
        security,
    });
    Ok(())
}

#[cfg(test)]
mod tests;
