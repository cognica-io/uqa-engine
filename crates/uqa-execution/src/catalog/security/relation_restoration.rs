//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind legacy relation security only inside complete initial catalog restoration.

use uqa_core::memory::{Budgeted, ProductionControl};
use uqa_sql::catalog::roles::RoleDefinitions;
use uqa_sql::catalog::security::{BoundTableSecurity, TableSecurity};
use uqa_storage::{
    CatalogFacade, RelationSecurityRow, StorageBackendError, StorageBackendResult, TableSchema,
};

pub fn restore_security(
    row: &RelationSecurityRow,
    columns: Option<&[String]>,
    roles: &RoleDefinitions,
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

/// Consume an admitted current-format authority without copying its ACL buffers or column nodes. Load-only restoration never binds legacy names or writes a migration.
pub fn restore_security_with_control<'a>(
    row: Budgeted<RelationSecurityRow>,
    columns: Option<impl Iterator<Item = &'a str> + Clone>,
    roles: &RoleDefinitions,
    control: &ProductionControl<'_>,
) -> StorageBackendResult<Budgeted<BoundTableSecurity>> {
    let security = current_security(row, control)?;
    security
        .validate_with_control(columns, roles, control)
        .map_err(security_error)?;
    Ok(security)
}

/// Retain native authority and validation scratch across the enclosing restore's work windows. Success transfers the original ACL allocations; interruption retires the entire unpublished authority.
pub async fn restore_security_from_input<'a>(
    row: Budgeted<RelationSecurityRow>,
    columns: Option<impl Iterator<Item = &'a str> + Clone>,
    roles: &RoleDefinitions,
    control: &ProductionControl<'_>,
    input: &mut uqa_core::image_io::AsyncImageInput<'_>,
) -> StorageBackendResult<Budgeted<BoundTableSecurity>> {
    let security = current_security(row, control)?;
    security
        .validate_from_input(columns, roles, control, input)
        .await
        .map_err(security_error)?;
    Ok(security)
}

fn current_security(
    row: Budgeted<RelationSecurityRow>,
    control: &ProductionControl<'_>,
) -> StorageBackendResult<Budgeted<BoundTableSecurity>> {
    control.check_cancellation()?;
    if !control
        .budget()
        .is_some_and(|memory| memory.shares_allowance(row.budget()))
    {
        return Err(StorageBackendError::Other(
            "stored relation security requires its original allowance".into(),
        ));
    }
    row.try_map(|row| {
        let RelationSecurityRow::Bound(row) = row else {
            return Err(StorageBackendError::Other(
                "relation security requires initial catalog migration".into(),
            ));
        };
        Ok(BoundTableSecurity::from_row(row))
    })
}

fn security_error(error: uqa_sql::SQLError) -> StorageBackendError {
    match error {
        uqa_sql::SQLError::Internal(message) => StorageBackendError::Other(message),
        uqa_sql::SQLError::Cancelled(error) => StorageBackendError::Cancelled(error),
        error => StorageBackendError::backend("stored relation security", error),
    }
}

pub fn restore_tables(
    catalog: &dyn CatalogFacade,
    roles: &RoleDefinitions,
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
