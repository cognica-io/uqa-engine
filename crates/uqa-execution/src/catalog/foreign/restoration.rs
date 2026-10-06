//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Restore foreign definitions and security inside the complete catalog transaction.

use crate::schema::sequences::owner_publication::SequenceOwnerPublicationContext;
use std::collections::BTreeMap;
use uqa_core::RelationIdentity;
use uqa_sql::{
    catalog::{roles::guards::RoleCatalogGuards, security::BoundTableSecurity},
    schema::foreign_tables::ForeignSchemaContext,
};
use uqa_storage::{CatalogFacade, StorageBackendError, StorageBackendResult};

pub struct ForeignRestoreContext<'a> {
    pub registry: &'a dyn super::reads::ForeignRegistryReads,
    pub schema: ForeignSchemaContext<'a>,
    pub sequences: SequenceOwnerPublicationContext<'a>,
    pub roles: &'a dyn RoleCatalogGuards,
}

pub struct RestoredForeignCatalog {
    pub servers: BTreeMap<String, uqa_sql::catalog::foreign_server::ForeignServerDefinition>,
    pub tables: BTreeMap<RelationIdentity, super::StoredForeignTable>,
    pub security: BTreeMap<RelationIdentity, BoundTableSecurity>,
}

/// Keep session-owned definitions available while the durable registries are rebound.
pub fn retain_temporary_registry(
    publication: &dyn crate::schema::foreign_table_alteration::ForeignTableAlterPublication,
) {
    retain_registry(publication, |table| {
        table.persistence == uqa_sql::ast::RelationPersistence::Temporary
    });
}

/// An independent session starts from durable foreign definitions, never another session's objects.
pub fn retain_durable_registry(
    publication: &dyn crate::schema::foreign_table_alteration::ForeignTableAlterPublication,
) {
    retain_registry(publication, |table| {
        table.persistence != uqa_sql::ast::RelationPersistence::Temporary
    });
}

fn retain_registry(
    publication: &dyn crate::schema::foreign_table_alteration::ForeignTableAlterPublication,
    mut keep: impl FnMut(&super::StoredForeignTable) -> bool,
) {
    let mut tables = publication.tables_write();
    tables.retain(|_, table| keep(table));
    publication
        .security_write()
        .retain(|relation, _| tables.contains_key(relation));
}

pub fn restore(
    context: &ForeignRestoreContext<'_>,
    catalog: &dyn CatalogFacade,
    allow_migration: bool,
) -> StorageBackendResult<RestoredForeignCatalog> {
    let restored_servers =
        super::servers::restore(catalog, &context.roles.role_definitions(), allow_migration)?;
    let servers = &restored_servers.definitions;
    let current_references = super::reference::check_format(catalog)?;
    let mut migrations = Vec::new();
    let mut tables = BTreeMap::new();
    let mut securities = BTreeMap::new();
    for row in catalog.load_foreign_tables()? {
        let relation_name = row.relation.qualified_name();
        let options: BTreeMap<String, String> = serde_json::from_str(&row.options_json)?;
        let (mut table, legacy_schema) = super::StoredForeignTable::from_catalog(
            relation_name.clone(),
            row.server_name.clone(),
            options,
            &row.columns_json,
        )?;
        let reference_migration =
            super::reference::restore(&mut table, servers, current_references, allow_migration)?;
        if table.object_id == [0; 16] {
            return Err(StorageBackendError::Other(format!(
                    "foreign table `{relation_name}` has no object identity and requires an initial-open migration"
                )));
        }
        uqa_sql::schema::constraint_metadata::identity::validate_not_null_identities(
            &table.columns,
        )
        .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        let schema_before_binding = table.schema_json()?;
        context
            .schema
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
        let schema_after_binding = table.schema_json()?;
        let schema_requires_migration =
            legacy_schema || reference_migration || schema_before_binding != schema_after_binding;
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
        context
            .sequences
            .validate_implicit_sequence_owners_for_columns(
                &relation_name,
                table.object_id,
                &table.columns,
            )?;
        if schema_requires_migration
            || matches!(row.security, uqa_storage::RelationSecurityRow::Legacy(_))
        {
            if !allow_migration {
                return Err(StorageBackendError::Other(format!(
                        "schema expressions on foreign table `{relation_name}` require an initial-open migration"
                    )));
            }
            migrations.push(table.catalog_row(&row.relation, &security)?);
        }
        tables.insert(row.relation.clone(), table);
        securities.insert(row.relation, security);
    }
    restored_servers.persist_migrations(catalog)?;
    for row in migrations {
        catalog.save_foreign_table(&row)?;
    }
    if !current_references && allow_migration {
        super::reference::initialize_format(catalog)?;
    }
    let mut restored = RestoredForeignCatalog {
        servers: restored_servers.definitions,
        tables,
        security: securities,
    };
    restored.retain_temporary(context.registry)?;
    Ok(restored)
}

impl RestoredForeignCatalog {
    fn retain_temporary(
        &mut self,
        registry: &dyn super::reads::ForeignRegistryReads,
    ) -> StorageBackendResult<()> {
        let tables = registry.tables();
        let security = registry.security();
        merge_temporary(&tables, &security, &mut self.tables, &mut self.security)
    }
}

/// Preserve the session's complete temporary definitions and security over a fresh durable snapshot.
pub fn merge_temporary(
    current: &BTreeMap<RelationIdentity, super::StoredForeignTable>,
    current_security: &BTreeMap<RelationIdentity, BoundTableSecurity>,
    tables: &mut BTreeMap<RelationIdentity, super::StoredForeignTable>,
    security: &mut BTreeMap<RelationIdentity, BoundTableSecurity>,
) -> StorageBackendResult<()> {
    for (relation, table) in current
        .iter()
        .filter(|(_, table)| table.persistence == uqa_sql::ast::RelationPersistence::Temporary)
    {
        let retained = current_security.get(relation).ok_or_else(|| {
            StorageBackendError::Other(format!(
                "temporary foreign table `{}` has no security metadata",
                relation.qualified_name()
            ))
        })?;
        tables.insert(relation.clone(), table.clone());
        security.insert(relation.clone(), retained.clone());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
