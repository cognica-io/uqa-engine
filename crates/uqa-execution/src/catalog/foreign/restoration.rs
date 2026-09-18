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
    pub schema: ForeignSchemaContext<'a>,
    pub sequences: SequenceOwnerPublicationContext<'a>,
    pub roles: &'a dyn RoleCatalogGuards,
}

pub struct RestoredForeignCatalog {
    pub servers: BTreeMap<String, uqa_fdw::ForeignServer>,
    pub tables: BTreeMap<RelationIdentity, super::StoredForeignTable>,
    pub security: BTreeMap<RelationIdentity, BoundTableSecurity>,
}

pub fn restore(
    context: &ForeignRestoreContext<'_>,
    catalog: &dyn CatalogFacade,
    allow_migration: bool,
) -> StorageBackendResult<RestoredForeignCatalog> {
    let servers = restore_servers(catalog)?;
    let mut migrations = Vec::new();
    let mut tables = BTreeMap::new();
    let mut securities = BTreeMap::new();
    for row in catalog.load_foreign_tables()? {
        let relation_name = row.relation.qualified_name();
        if !servers.contains_key(&row.server_name) {
            return Err(StorageBackendError::Other(format!(
                "foreign table `{}` references missing server `{}`",
                relation_name, row.server_name
            )));
        }
        let options: BTreeMap<String, String> = serde_json::from_str(&row.options_json)?;
        let (mut table, legacy_schema) = super::StoredForeignTable::from_catalog(
            relation_name.clone(),
            row.server_name.clone(),
            options,
            &row.columns_json,
        )?;
        if table.object_id == [0; 16] {
            return Err(StorageBackendError::Other(format!(
                    "foreign table `{relation_name}` has no object identity and requires an initial-open migration"
                )));
        }
        let schema_before_binding = table.schema_json()?;
        context
            .schema
            .prepare_stored_foreign_table_schema(
                &relation_name,
                &mut table.columns,
                &mut table.checks,
            )
            .map_err(|error| {
                StorageBackendError::Other(format!(
                    "restore foreign table `{relation_name}` schema: {error}"
                ))
            })?;
        let schema_after_binding = table.schema_json()?;
        let schema_requires_migration =
            legacy_schema || schema_before_binding != schema_after_binding;
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
    for row in migrations {
        catalog.save_foreign_table(&row)?;
    }
    Ok(RestoredForeignCatalog {
        servers,
        tables,
        security: securities,
    })
}

fn restore_servers(
    catalog: &dyn CatalogFacade,
) -> StorageBackendResult<BTreeMap<String, uqa_fdw::ForeignServer>> {
    let mut servers = BTreeMap::new();
    for (name, fdw_type, options_json) in catalog.load_foreign_servers()? {
        let options: BTreeMap<String, String> = serde_json::from_str(&options_json)?;
        servers.insert(
            name.clone(),
            uqa_fdw::ForeignServer {
                name,
                fdw_type,
                options,
            },
        );
    }
    Ok(servers)
}
