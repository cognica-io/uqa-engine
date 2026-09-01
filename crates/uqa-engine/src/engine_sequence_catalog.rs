//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable sequence catalog conversion, migration, and registry hydration.

use super::{
    BTreeMap, CatalogFacade, Engine, RelationIdentity, SequenceDataType, SequenceOptions,
    SequenceRow, SequenceState, StorageBackendError, StorageBackendResult, SEQUENCES_METADATA_KEY,
};
use crate::engine_state::SequenceSecurity;

impl Engine {
    pub(crate) fn sequence_row(
        name: &str,
        object_id: [u8; 16],
        state: SequenceState,
        persistence: uqa_sql::ast::RelationPersistence,
        security: &SequenceSecurity,
    ) -> StorageBackendResult<SequenceRow> {
        Ok(SequenceRow {
            relation: RelationIdentity::from_legacy_name(name)
                .map_err(StorageBackendError::Other)?,
            role_owner: security.role_owner.clone(),
            acl: security.acl.clone(),
            object_id,
            definition_generation: state.definition_generation,
            start: state.start,
            increment: state.increment,
            current: state.current,
            called: state.called,
            persistence: persistence.catalog_code().into(),
            owner: state.owner,
            options: SequenceOptions {
                data_type: state.data_type.sql_name().into(),
                min_value: Some(state.min_value),
                max_value: Some(state.max_value),
                cycle: state.cycle,
                cache_size: state.cache_size,
            },
        })
    }

    pub(crate) fn refresh_sequences_from_catalog(&self) -> StorageBackendResult<()> {
        let sequence_session = self.open_nontransactional_sequence_session()?;
        let catalog = sequence_session
            .as_ref()
            .map(|session| session.catalog.as_ref())
            .or(self.storage.catalog.as_deref());
        let Some(catalog) = catalog else {
            return Ok(());
        };
        let rows = catalog.load_sequence_rows()?;
        self.install_durable_sequence_rows(rows)?;
        Ok(())
    }

    /// Consume the legacy all-sequences metadata snapshot during initial
    /// engine open. Runtime catalog reloads must never call this migration:
    /// they can run inside a pinned read transaction or after a physical
    /// rollback, where clearing metadata would silently turn restoration into
    /// a database write.
    pub(crate) fn migrate_legacy_sequences_from_metadata(
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        // One-time, restart-safe migration from the former all-sequences JSON
        // snapshot. Merge idempotently even after a partially completed run,
        // then clear the legacy payload so deliberately dropping every typed
        // sequence cannot resurrect the old snapshot on the next open.
        if let Some(json) = catalog.get_metadata(SEQUENCES_METADATA_KEY)? {
            let legacy = serde_json::from_str::<BTreeMap<String, SequenceState>>(&json)?;
            if !legacy.is_empty() {
                for (name, state) in legacy {
                    catalog.create_sequence_row(&Self::sequence_row(
                        &name,
                        crate::new_sequence_object_id()?,
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

    pub(crate) fn migrate_sequence_identities(
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        let mut identities = std::collections::BTreeSet::new();
        for mut row in catalog.load_sequence_rows()? {
            let mut changed = false;
            if row.object_id == [0; 16] || !identities.insert(row.object_id) {
                loop {
                    let object_id = crate::new_sequence_object_id()?;
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

    /// Restore the typed sequence registry without modifying the catalog.
    /// This is safe for initial hydration, pinned snapshots, external-commit
    /// refreshes, and rollback cleanup alike.
    pub(crate) fn restore_sequences_from_catalog(
        &self,
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        let rows = catalog.load_sequence_rows()?;
        self.install_durable_sequence_rows(rows)?;
        Ok(())
    }

    fn install_durable_sequence_rows(&self, rows: Vec<SequenceRow>) -> StorageBackendResult<()> {
        let temporary_persistence = self
            .durable
            .sequence_persistence
            .read()
            .iter()
            .filter(|(_, persistence)| {
                **persistence == uqa_sql::ast::RelationPersistence::Temporary
            })
            .map(|(relation, persistence)| (relation.clone(), *persistence))
            .collect::<BTreeMap<_, _>>();
        let mut sequences = self
            .durable
            .sequences
            .read()
            .iter()
            .filter(|(relation, _)| temporary_persistence.contains_key(*relation))
            .map(|(relation, state)| (relation.clone(), *state))
            .collect::<BTreeMap<_, _>>();
        let mut object_ids = self
            .durable
            .sequence_object_ids
            .read()
            .iter()
            .filter(|(relation, _)| temporary_persistence.contains_key(*relation))
            .map(|(relation, object_id)| (relation.clone(), *object_id))
            .collect::<BTreeMap<_, _>>();
        let mut security = self
            .durable
            .sequence_security
            .read()
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
            let (relation, state) = Self::sequence_state_from_row(row)?;
            persistence.insert(relation.clone(), stored);
            object_ids.insert(relation.clone(), object_id);
            security.insert(relation.clone(), SequenceSecurity { role_owner, acl });
            sequences.insert(relation, state);
        }
        *self.durable.sequences.write() = sequences;
        *self.durable.sequence_object_ids.write() = object_ids;
        *self.durable.sequence_persistence.write() = persistence;
        *self.durable.sequence_security.write() = security;
        Ok(())
    }

    fn sequence_state_from_row(
        row: SequenceRow,
    ) -> StorageBackendResult<(RelationIdentity, SequenceState)> {
        if row.increment == 0 {
            return Err(StorageBackendError::Other(format!(
                "corrupt sequence `{}` has zero increment",
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
            data_type,
            min_value: row.options.min_value.unwrap_or(if row.increment > 0 {
                1
            } else {
                type_min
            }),
            max_value: row.options.max_value.unwrap_or(if row.increment > 0 {
                type_max
            } else {
                -1
            }),
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
        Self::validate_sequence_definition(state, false).map_err(|error| {
            StorageBackendError::Other(format!(
                "corrupt sequence `{}` definition: {error}",
                row.relation.qualified_name()
            ))
        })?;
        Ok((row.relation, state))
    }
}
