//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Restore foreign definitions and security inside the complete catalog transaction.

use crate::schema::sequences::owner_publication::SequenceOwnerPublicationContext;
use std::collections::{BTreeMap, BTreeSet};
use uqa_core::RelationIdentity;
use uqa_sql::{
    catalog::{roles::guards::RoleCatalogGuards, security::BoundTableSecurity},
    schema::foreign_tables::ForeignSchemaContext,
};
use uqa_storage::{CatalogFacade, StorageBackendError, StorageBackendResult};

mod declarations;

pub struct ForeignRestoreContext<'a> {
    pub registry: &'a dyn super::reads::ForeignRegistryReads,
    pub publication: &'a dyn crate::schema::foreign_table_alteration::ForeignTableAlterPublication,
    pub schema: ForeignSchemaContext<'a>,
    pub sequences: SequenceOwnerPublicationContext<'a>,
    pub roles: &'a dyn RoleCatalogGuards,
    pub routines: &'a dyn crate::routines::catalog::RoutineRegistryState,
}

pub struct RestoredForeignCatalog {
    pub wrappers: uqa_sql::catalog::foreign_wrapper::ForeignWrappers,
    pub servers: uqa_sql::catalog::foreign_server::ForeignServers,
    pub relations: super::ForeignCatalog,
}

/// Keep session-owned definitions available while the durable registries are rebound.
pub fn retain_session_registry(
    publication: &dyn crate::schema::foreign_table_alteration::ForeignTableAlterPublication,
) -> StorageBackendResult<()> {
    retain_registry(publication, retain_in_session)
}

/// An independent session retains only complete durable declarations.
pub fn retain_durable_registry(
    publication: &dyn crate::schema::foreign_table_alteration::ForeignTableAlterPublication,
) -> StorageBackendResult<()> {
    retain_registry(publication, |table| {
        table.persistence != uqa_sql::ast::RelationPersistence::Temporary
            && table.server_reference.is_some()
    })
}

fn retain_registry(
    publication: &dyn crate::schema::foreign_table_alteration::ForeignTableAlterPublication,
    mut keep: impl FnMut(&super::StoredForeignTable) -> bool,
) -> StorageBackendResult<()> {
    let mut next = publication.foreign_catalog();
    next.retain(|_, table| keep(table))
        .map_err(|error| StorageBackendError::backend("retain foreign definitions", error))?;
    publication.publish_foreign_catalog(next);
    Ok(())
}

pub(super) fn admit_restored(
    tables: &BTreeMap<RelationIdentity, super::StoredForeignTable>,
    security: &BTreeMap<RelationIdentity, BoundTableSecurity>,
    budget: &uqa_core::memory::MemoryBudget,
) -> StorageBackendResult<super::ForeignCatalog> {
    let mut next = super::ForeignCatalog::with_budget(budget);
    for (relation, table) in tables {
        let authority = security.get(relation).ok_or_else(|| {
            StorageBackendError::Other(format!(
                "foreign table `{}` has no authority metadata",
                relation.qualified_name()
            ))
        })?;
        next.insert(relation.clone(), table.clone(), authority.clone())
            .map_err(|error| {
                StorageBackendError::backend("admit restored foreign relation", error)
            })?;
    }
    if security.len() != tables.len() {
        return Err(StorageBackendError::Other(
            "foreign authority has no matching definition".into(),
        ));
    }
    Ok(next)
}

pub fn restore(
    context: &ForeignRestoreContext<'_>,
    catalog: &dyn CatalogFacade,
    allow_migration: bool,
) -> StorageBackendResult<RestoredForeignCatalog> {
    let mut restored_servers =
        super::servers::restore(catalog, &context.roles.role_definitions(), allow_migration)?;
    let restored_wrappers = restore_wrappers(
        context,
        catalog,
        &mut restored_servers.definitions,
        allow_migration,
    )?;
    let servers = &restored_servers.definitions;
    let reference_format = super::reference::restore_format(catalog, allow_migration)?;
    let current_references = reference_format.is_some();
    let mut pending_migrations = BTreeSet::new();
    let mut tables = BTreeMap::new();
    let mut securities = BTreeMap::new();
    for row in catalog.load_foreign_tables()? {
        let relation_name = row.relation.qualified_name();
        let (mut table, legacy_schema) =
            super::StoredForeignTable::from_catalog_row(&row, reference_format)?;
        let reference_migration =
            super::reference::restore(&mut table, servers, current_references, allow_migration)?;
        if table.object_id == [0; 16] {
            return Err(StorageBackendError::Other(format!(
                    "foreign table `{relation_name}` has no object identity and requires an initial-open migration"
                )));
        }
        let column_names = table
            .columns
            .iter()
            .map(|column| column.name.clone())
            .collect::<Vec<_>>();
        let security = crate::catalog::security::relation_restoration::restore_security(
            &row.security,
            Some(&column_names),
            &context.roles.role_definitions(),
            allow_migration,
        )?;
        if legacy_schema
            || reference_migration
            || matches!(row.security, uqa_storage::RelationSecurityRow::Legacy(_))
        {
            pending_migrations.insert(row.relation.clone());
        }
        tables.insert(row.relation.clone(), table);
        securities.insert(row.relation, security);
    }
    let declarations = declarations::ForeignDeclarations::new(context, &tables, &securities)?;
    let mut migrations = Vec::new();
    for (relation, table) in &mut tables {
        let relation_name = relation.qualified_name();
        let changed_schema = restore_schema(&context.schema, relation, table)?;
        context
            .sequences
            .validate_implicit_sequence_owners_for_columns(
                &relation_name,
                table.object_id,
                &table.columns,
            )?;
        if changed_schema || pending_migrations.contains(relation) {
            if !allow_migration {
                return Err(StorageBackendError::Other(format!(
                        "schema expressions on foreign table `{relation_name}` require an initial-open migration"
                    )));
            }
            let security = securities.get(relation).ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "foreign table `{relation_name}` lost its security metadata"
                ))
            })?;
            migrations.push(table.catalog_row(relation, security)?);
        }
    }
    drop(declarations);
    let current = context.publication.foreign_catalog();
    let mut relations = admit_restored(&tables, &securities, current.budget())?;
    merge_session_relations(&current, &mut relations)?;
    restored_servers.persist_migrations(catalog)?;
    restored_wrappers.persist_migrations(catalog)?;
    for row in migrations {
        catalog.save_foreign_table(&row)?;
    }
    if reference_format != Some(2) && allow_migration {
        super::reference::initialize_format(catalog)?;
    }
    Ok(RestoredForeignCatalog {
        wrappers: restored_wrappers.definitions,
        servers: restored_servers.definitions,
        relations,
    })
}

fn restore_schema(
    schema: &ForeignSchemaContext<'_>,
    relation: &RelationIdentity,
    table: &mut super::StoredForeignTable,
) -> StorageBackendResult<bool> {
    uqa_sql::schema::constraint_metadata::identity::validate_not_null_identities(&table.columns)
        .map_err(|error| StorageBackendError::Other(error.to_string()))?;
    let before = table.schema_json()?;
    let relation_name = relation.qualified_name();
    schema
        .prepare_stored_foreign_table_schema(
            &relation_name,
            &mut table.columns,
            &mut table.checks,
            &table.dropped_attributes,
            &mut crate::catalog::identity::allocate_catalog_object_id,
        )
        .map_err(|error| {
            StorageBackendError::Other(format!(
                "restore foreign table `{relation_name}` schema: {error}"
            ))
        })?;
    Ok(before != table.schema_json()?)
}

fn restore_wrappers(
    context: &ForeignRestoreContext<'_>,
    catalog: &dyn CatalogFacade,
    servers: &mut uqa_sql::catalog::foreign_server::ForeignServers,
    allow_migration: bool,
) -> StorageBackendResult<super::wrappers::RestoredWrappers> {
    let restored = super::wrappers::restore(
        catalog,
        &context.roles.role_definitions(),
        servers,
        allow_migration,
    )?;
    uqa_sql::catalog::foreign_wrapper::validate_functions(
        &restored.definitions,
        &context.routines.routine_snapshot(),
    )
    .map_err(|error| StorageBackendError::Other(error.to_string()))?;
    Ok(restored)
}

/// Session-local relations and incomplete declarations remain in the same paired generation.
pub fn merge_session_relations(
    current: &super::ForeignCatalog,
    next: &mut super::ForeignCatalog,
) -> StorageBackendResult<()> {
    let mut candidate = next.clone();
    for (relation, table) in current
        .tables()
        .iter()
        .filter(|(_, table)| retain_in_session(table))
    {
        candidate
            .insert(
                relation.clone(),
                table.clone(),
                current.security()[relation].clone(),
            )
            .map_err(|error| {
                StorageBackendError::backend("retain session foreign relation", error)
            })?;
    }
    *next = candidate;
    Ok(())
}

fn retain_in_session(table: &super::StoredForeignTable) -> bool {
    table.persistence == uqa_sql::ast::RelationPersistence::Temporary
        || table.server_reference.is_none()
}

#[cfg(test)]
mod tests;
