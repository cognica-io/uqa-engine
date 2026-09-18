//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind legacy relation security only inside complete initial catalog restoration.

use std::collections::BTreeMap;
use uqa_sql::catalog::{
    roles::RoleDefinition,
    security::{BoundTableSecurity, TableSecurity},
};
use uqa_storage::{
    CatalogFacade, RelationSecurityRow, StorageBackendError, StorageBackendResult, TableSchema,
};

pub fn restore_security(
    row: &RelationSecurityRow,
    columns: Option<&[String]>,
    roles: &BTreeMap<String, RoleDefinition>,
    allow_migration: bool,
) -> StorageBackendResult<BoundTableSecurity> {
    let security = match row {
        RelationSecurityRow::Bound(row) => BoundTableSecurity::from_row(row.clone()),
        RelationSecurityRow::Legacy(row) => {
            if !allow_migration {
                return Err(StorageBackendError::Other(
                    "relation security requires initial catalog migration".into(),
                ));
            }
            BoundTableSecurity::bind(&TableSecurity::from_legacy(row.clone()), roles)
                .map_err(StorageBackendError::Other)?
        }
    };
    security
        .validate(columns, roles)
        .map_err(StorageBackendError::Other)?;
    Ok(security)
}

pub fn restore_tables(
    catalog: &dyn CatalogFacade,
    roles: &BTreeMap<String, RoleDefinition>,
    allow_migration: bool,
) -> StorageBackendResult<Vec<(TableSchema, BoundTableSecurity)>> {
    let mut restored = Vec::new();
    let mut migrations = Vec::new();
    for mut schema in catalog.load_tables()? {
        let columns: Vec<uqa_sql::ast::ColumnDef> = if schema.columns_json.is_empty() {
            Vec::new()
        } else {
            serde_json::from_str(&schema.columns_json)?
        };
        let columns = columns
            .into_iter()
            .map(|column| column.name)
            .collect::<Vec<_>>();
        let security = restore_security(&schema.security, Some(&columns), roles, allow_migration)
            .map_err(|error| {
            StorageBackendError::Other(format!(
                "table `{}` has invalid security metadata: {error}",
                schema.relation.qualified_name()
            ))
        })?;
        if matches!(schema.security, RelationSecurityRow::Legacy(_)) {
            schema.security = security.row().into();
            migrations.push(restored.len());
        }
        restored.push((schema, security));
    }
    for index in migrations {
        catalog.save_table(&restored[index].0)?;
    }
    Ok(restored)
}

#[cfg(test)]
mod tests;
