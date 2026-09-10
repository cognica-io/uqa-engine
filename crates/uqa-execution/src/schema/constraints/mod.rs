//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Execute constraint lifecycle operations against scoped metadata, row, and publication services.
use crate::mutation::constraints::context::ConstraintContext;
use crate::schema::{
    hierarchy::{HierarchyCatalog, HierarchyNamespace},
    publication::{SchemaPublicationContext, SchemaWriteTransaction},
};
pub use uqa_sql::schema::constraint_changes::{
    constraint_error, ensure_constraint_name_available, ensure_not_null_inheritable,
    find_constraint, ConstraintLocation,
};
use uqa_sql::{
    ast::{ColumnDef, ForeignKey, TableHierarchy},
    catalog::constraints::ConstraintIdentity,
    schema::{foreign_keys::ForeignKeyDefinitionContext, indexes::names::IndexNameCatalog},
    SQLError,
};
use uqa_storage::{StorageBackendError, StorageBackendResult};
pub trait ConstraintRelations {
    fn table_names(&self) -> StorageBackendResult<Vec<String>>;
    fn table_hierarchy(&self, table: &str) -> StorageBackendResult<TableHierarchy>;
}
pub trait ConstraintAlterAccess {
    fn ensure_table_owner(&self, table: &str) -> Result<(), SQLError>;
    fn ensure_no_pending_events(&self, table: &str, action: &str) -> Result<(), SQLError>;
    fn constraint_trigger_name(&self, table: &str, name: &str) -> Result<Option<String>, SQLError>;
}
pub trait ConstraintModes {
    fn prune(&self) -> Result<(), SQLError>;
    fn forget(&self, identity: &ConstraintIdentity);
}
pub struct ConstraintAlterContext<'a> {
    pub catalog: &'a dyn HierarchyCatalog,
    pub relations: &'a dyn ConstraintRelations,
    pub access: &'a dyn ConstraintAlterAccess,
    pub locks: &'a dyn HierarchyNamespace,
    pub modes: &'a dyn ConstraintModes,
    pub names: &'a dyn IndexNameCatalog,
    pub rows: ConstraintContext<'a>,
    pub foreign_keys: ForeignKeyDefinitionContext<'a>,
    pub publication: SchemaPublicationContext<'a>,
    pub writes: &'a dyn SchemaWriteTransaction,
    pub notices: &'a parking_lot::Mutex<Vec<(String, String)>>,
}
fn ddl_storage_error(action: &str, error: StorageBackendError) -> SQLError {
    uqa_sql::catalog::errors::storage_error(action, &error)
}
pub mod checks;
pub mod drop;
mod lifecycle;
pub use lifecycle::*;

pub fn table_constraint_state(
    context: &ConstraintAlterContext<'_>,
    table: &str,
) -> Result<
    (
        Vec<uqa_sql::ast::ColumnDef>,
        uqa_sql::ast::TableConstraintSet,
    ),
    SQLError,
> {
    let columns = context
        .catalog
        .try_describe_table(table)
        .map_err(|error| ddl_storage_error("ALTER TABLE constraint state", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let constraints = context
        .catalog
        .try_declared_table_constraints(table)
        .map_err(|error| ddl_storage_error("ALTER TABLE constraint state", error))?;
    Ok((columns, constraints))
}

fn materialize_constraint_candidate(
    context: &ConstraintAlterContext<'_>,
    table: &str,
    columns: &mut [uqa_sql::ast::ColumnDef],
    constraints: &mut uqa_sql::ast::TableConstraintSet,
) -> Result<(), SQLError> {
    let canonical = context
        .publication
        .catalog
        .resolve_table_name(table)
        .map_err(|error| ddl_storage_error("ALTER TABLE constraint naming", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let relation = uqa_core::RelationIdentity::from_legacy_name(&canonical).map_err(|message| {
        SQLError::Internal(format!("constraint relation identity: {message}"))
    })?;
    uqa_sql::schema::indexes::names::name_constraint_indexes(
        context.names,
        &canonical,
        &mut constraints.key_constraints,
    )?;
    let mut allocate = context.publication.allocate_identity;
    uqa_sql::schema::constraint_metadata::materialize_constraint_metadata(
        &relation,
        columns,
        constraints,
        &mut allocate,
    )
    .map_err(|error| {
        ddl_storage_error(
            "ALTER TABLE constraint naming",
            StorageBackendError::Other(error.to_string()),
        )
    })?;
    Ok(())
}

pub fn publish_constraint_state(
    context: &ConstraintAlterContext<'_>,
    table: &str,
    columns: Vec<uqa_sql::ast::ColumnDef>,
    constraints: uqa_sql::ast::TableConstraintSet,
) -> Result<(), SQLError> {
    context
        .writes
        .with_schema_write(Box::new(|publication| {
            crate::schema::publication::replace_constraint_state(
                publication,
                table,
                columns,
                constraints,
            )
        }))
        .map_err(|error| ddl_storage_error("ALTER TABLE constraint catalog", error))?;
    context.modes.prune()
}

fn validate_check_expression(
    context: &ConstraintAlterContext<'_>,
    table: &str,
    qualifier: &str,
    columns: &[ColumnDef],
    expression: &mut uqa_sql::ast::Expr,
) -> Result<(), SQLError> {
    let binding = context.publication.bindings.bindings.binding_scope()?;
    uqa_sql::schema::constraints::validate_check_expression(
        &uqa_sql::schema::SchemaBindingContext {
            catalog: context.publication.bindings.schema,
            binding: &binding.context(),
        },
        table,
        qualifier,
        columns,
        expression,
    )
}
fn foreign_key_constraint_identity(
    context: &ConstraintAlterContext<'_>,
    table: &str,
    foreign_key: &ForeignKey,
) -> Result<ConstraintIdentity, SQLError> {
    let canonical = context
        .publication
        .catalog
        .resolve_table_name(table)
        .map_err(|error| {
            SQLError::Internal(format!(
                "resolve foreign-key relation '{table}' for constraint mode: {error}"
            ))
        })?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    uqa_sql::catalog::constraints::foreign_key_identity(&canonical, foreign_key)
}
