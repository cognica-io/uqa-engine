//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Restore durable sequence metadata and migrate legacy rows inside the caller's open transaction.
use super::{
    registry::{SequenceCatalog, SequenceCatalogRead, SequenceRecord},
    sequence_row, SequenceState,
};
use std::collections::BTreeMap;
use uqa_core::RelationIdentity;
use uqa_sql::catalog::roles::RoleDefinitions;
use uqa_sql::{
    ast::{RelationPersistence, SequenceDataType},
    catalog::security::BoundSequenceSecurity,
};
use uqa_storage::{CatalogFacade, SequenceRow, StorageBackendError, StorageBackendResult};
mod security;
pub const SEQUENCES_METADATA_KEY: &str = "sql_sequences_json";
pub type RestoredSequenceRegistry = SequenceCatalog;

pub trait SequenceRestoreRegistry {
    fn read(&self) -> SequenceCatalogRead<'_>;
    fn install(&self, registry: SequenceCatalog);
}
pub struct SequenceRestoreContext<'a> {
    pub registry: &'a dyn SequenceRestoreRegistry,
    pub roles: &'a dyn uqa_sql::catalog::roles::guards::RoleCatalogGuards,
}

fn catalog_error(error: uqa_sql::SQLError) -> StorageBackendError {
    StorageBackendError::backend("sequence catalog", error)
}

/// Sequence values use current committed rows while transaction-private creation, rename, replacement and deletion keep their selected scope. This does not advance the caller's ordinary row snapshot.
pub fn load_sequence_value_rows(
    bound: &dyn CatalogFacade,
    committed: &dyn CatalogFacade,
) -> StorageBackendResult<Vec<SequenceRow>> {
    select_sequence_value_rows(
        bound.load_sequence_rows()?,
        committed.load_sequence_rows()?,
        |row| bound.sequence_has_private_changes(&row.relation, row.object_id),
    )
}

fn select_sequence_value_rows(
    bound: Vec<SequenceRow>,
    committed: Vec<SequenceRow>,
    mut private: impl FnMut(&SequenceRow) -> StorageBackendResult<bool>,
) -> StorageBackendResult<Vec<SequenceRow>> {
    // Value operations move the value record of a committed generation outside every transaction, so its committed state is newer than what the caller's transaction may have read, also for a definition the transaction changed.
    let committed_values = committed
        .iter()
        .map(|row| {
            (
                (row.object_id, row.definition_generation),
                (row.current, row.called, row.log_count),
            )
        })
        .collect::<std::collections::HashMap<_, _>>();
    select_sequence_records(
        bound.into_iter().map(|row| (row.relation.clone(), row)),
        committed.into_iter().map(|row| (row.relation.clone(), row)),
        |_, row| private(row),
    )
    .map(|rows| {
        rows.into_values()
            .map(|mut row| {
                if let Some(&(current, called, log_count)) =
                    committed_values.get(&(row.object_id, row.definition_generation))
                {
                    row.current = current;
                    row.called = called;
                    row.log_count = log_count;
                }
                row
            })
            .collect()
    })
}

/// Select complete records from the committed catalog or the caller's private view, including renamed and deleted records.
pub(super) fn select_sequence_records<T>(
    bound: impl IntoIterator<Item = (RelationIdentity, T)>,
    committed: impl IntoIterator<Item = (RelationIdentity, T)>,
    mut private: impl FnMut(&RelationIdentity, &T) -> StorageBackendResult<bool>,
) -> StorageBackendResult<BTreeMap<RelationIdentity, T>> {
    let mut rows = BTreeMap::new();
    for (relation, row) in committed {
        if !private(&relation, &row)? {
            rows.insert(relation, row);
        }
    }
    for (relation, row) in bound {
        if private(&relation, &row)? {
            rows.insert(relation, row);
        }
    }
    Ok(rows)
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
                    &BoundSequenceSecurity::owner(uqa_core::catalog_role::RoleIdentity::BOOTSTRAP),
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
/// Initial restoration validates the complete candidate before writing conversions or publishing registries.
/// The caller retains the complete catalog-open transaction until all other catalogs validate.
pub fn restore_sequence_catalog(
    context: &SequenceRestoreContext<'_>,
    catalog: &dyn CatalogFacade,
    allow_migration: bool,
) -> StorageBackendResult<()> {
    let roles = context.roles.role_definitions();
    let current = context.registry.read();
    let temporary = current.temporary_only();
    let (mut registry, migrations) = prepare_sequence_rows_with_migration(
        temporary,
        catalog.load_sequence_rows()?,
        &roles,
        allow_migration,
    )?;
    let mut recorded = super::catalog_oids::load(catalog)?;
    for (_, record) in current.iter() {
        if record.persistence == RelationPersistence::Temporary {
            if let Some(oid) = record.catalog_oid {
                recorded.insert(record.object_id, oid);
            }
        }
    }
    registry
        .replace_catalog_oids(recorded)
        .map_err(catalog_error)?;
    drop(current);
    for row in migrations {
        if !catalog.replace_sequence_row(&row)? {
            return Err(StorageBackendError::Other(format!(
                "sequence `{}` disappeared during security migration",
                row.relation.qualified_name()
            )));
        }
    }
    context.registry.install(registry);
    Ok(())
}

pub fn restore_sequence_rows(
    context: &SequenceRestoreContext<'_>,
    rows: Vec<SequenceRow>,
) -> StorageBackendResult<()> {
    let temporary = context.registry.read().temporary_only();
    let registry = prepare_sequence_rows(temporary, rows, &context.roles.role_definitions())?;
    context.registry.install(registry);
    Ok(())
}

pub(super) fn prepare_sequence_rows(
    temporary: RestoredSequenceRegistry,
    rows: Vec<SequenceRow>,
    roles: &RoleDefinitions,
) -> StorageBackendResult<RestoredSequenceRegistry> {
    prepare_sequence_rows_with_migration(temporary, rows, roles, false)
        .map(|(registry, _)| registry)
}

fn prepare_sequence_rows_with_migration(
    temporary: RestoredSequenceRegistry,
    rows: Vec<SequenceRow>,
    roles: &RoleDefinitions,
    allow_migration: bool,
) -> StorageBackendResult<(RestoredSequenceRegistry, Vec<SequenceRow>)> {
    let mut migrations = Vec::new();
    let mut registry = temporary;
    for authority in registry.security().values() {
        authority
            .validate(roles)
            .map_err(StorageBackendError::Other)?;
    }
    let mut seen_object_ids = registry
        .iter()
        .map(|(_, record)| record.object_id)
        .collect::<std::collections::BTreeSet<_>>();
    for mut row in rows {
        let name = row.relation.qualified_name();
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
        let authority =
            security::restore_security(&row.security, roles, allow_migration).map_err(|error| {
                StorageBackendError::Other(format!(
                    "corrupt sequence `{name}` has invalid security metadata: {error}"
                ))
            })?;
        if matches!(row.security, uqa_storage::SequenceSecurityRow::Legacy(_)) {
            row.security = authority.row().into();
            migrations.push(row.clone());
        }
        let (relation, state) = sequence_state_from_row(row)?;
        registry
            .insert(
                relation,
                SequenceRecord {
                    state,
                    object_id,
                    persistence: stored,
                    catalog_oid: None,
                },
                authority,
            )
            .map_err(catalog_error)?;
    }
    Ok((registry, migrations))
}

#[cfg(test)]
mod tests;
