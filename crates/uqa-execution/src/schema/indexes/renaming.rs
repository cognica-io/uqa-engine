//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rename an already bound index without replacing its identities, edges or physical namespace.

use super::registry::{IndexRegistryChange, IndexRegistryContext};
use crate::schema::constraints::{renaming::rename_constraint, ConstraintAlterContext};
use crate::schema::table_alteration::binding::TableAlterBindingContext;
use uqa_core::RelationIdentity;
use uqa_sql::{
    catalog::errors::storage_error, schema::relation_alteration::relation_rename_target, SQLError,
    SQLResult,
};
use uqa_storage::{StorageBackendError, StorageBackendResult};

/// Publish the two names through their shared durable index row without rewriting the table's key structure.
pub(crate) fn rename_owned_constraint(
    context: &IndexRegistryContext<'_>,
    table: &str,
    owner: [u8; 16],
    new_name: &str,
    columns: Vec<uqa_sql::ast::ColumnDef>,
    constraints: uqa_sql::ast::TableConstraintSet,
) -> StorageBackendResult<()> {
    let catalog = context.identities.catalog.current_catalog_snapshot();
    let row = catalog
        .catalog_indexes()
        .filter(|row| row.table_name == table)
        .find_map(|row| match crate::catalog::index::index_definition(row) {
            Ok(definition) if definition.relationships.owning_constraint == Some(owner) => {
                Some(Ok(row))
            }
            Ok(_) => None,
            Err(error) => Some(Err(error)),
        })
        .transpose()?
        .ok_or_else(|| StorageBackendError::Other("constraint has no owned index".into()))?;
    let mut renamed = row.clone();
    renamed.relation.name = new_name.into();
    IndexRegistryChange::rename(context, row, renamed)?;
    context
        .tables
        .table_state(table)?
        .ok_or_else(|| StorageBackendError::Other("index owner disappeared".into()))?
        .publish_constraints(columns, constraints);
    Ok(())
}

pub struct IndexRenameContext<'a> {
    pub registry: IndexRegistryContext<'a>,
    pub constraints: ConstraintAlterContext<'a>,
    pub binding: TableAlterBindingContext<'a>,
}

pub type IndexRenameWrite<'a> =
    Box<dyn FnOnce(&IndexRenameContext<'_>) -> Result<SQLResult, SQLError> + 'a>;

pub trait IndexRenameTransactions {
    fn with_index_rename(&self, write: IndexRenameWrite<'_>) -> Result<SQLResult, SQLError>;
}

pub fn rename_bound_index(
    context: &IndexRenameContext<'_>,
    name: &str,
    new_name: &str,
) -> Result<(), SQLError> {
    let relation = RelationIdentity::from_legacy_name(name).map_err(SQLError::Internal)?;
    context.binding.locks.prepare_definition_write()?;
    crate::schema::relation_alteration::validate_relation_alter_authority(
        &context.binding.authority,
        &context.binding.creation,
        &relation,
        "index",
        true,
    )?;
    let target = relation_rename_target(
        context.binding.names,
        &relation,
        new_name,
        "ALTER INDEX RENAME",
    )?;
    let catalog = context
        .registry
        .identities
        .catalog
        .current_catalog_snapshot();
    let row = catalog
        .snapshot()
        .definitions
        .catalog_indexes
        .get(&relation)
        .ok_or_else(|| SQLError::Routine {
            sqlstate: "42P01".into(),
            message: format!("relation \"{}\" does not exist", relation.name),
        })?;
    let definition = crate::catalog::index::index_definition(row)
        .map_err(|error| storage_error("ALTER INDEX RENAME", &error))?;
    if definition.relationships.owning_constraint.is_some() {
        if !rename_constraint(
            &context.constraints,
            &row.table_name,
            &relation.name,
            &target.name,
            false,
        )? {
            return Err(SQLError::Internal(
                "index owning constraint disappeared".into(),
            ));
        }
    } else {
        let mut renamed = row.clone();
        renamed.relation = target;
        IndexRegistryChange::rename(&context.registry, row, renamed)
            .map_err(|error| storage_error("ALTER INDEX RENAME", &error))?;
    }
    Ok(())
}
