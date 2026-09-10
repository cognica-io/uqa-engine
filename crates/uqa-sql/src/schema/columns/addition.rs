//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind an added column against current defaults, CHECKs, generated expressions, and foreign keys.
use crate::{
    assignment::columns::ColumnCatalogError,
    ast::{ColumnDef, ForeignKey, TableKeyConstraint},
    schema::{
        foreign_keys::ForeignKeyDefinitionContext, SchemaBindingContext, SchemaExpressionCatalog,
    },
    semantics::conflict::InferenceBindingScope,
    SQLError,
};
pub trait AddedColumnKeys {
    fn try_key_constraints(
        &self,
        table: &str,
    ) -> Result<Vec<TableKeyConstraint>, ColumnCatalogError>;
    fn try_foreign_keys(&self, table: &str) -> Result<Vec<ForeignKey>, ColumnCatalogError>;
}
pub struct AddedColumnAnalysisContext<'a> {
    pub keys: &'a dyn AddedColumnKeys,
    pub schema: &'a dyn SchemaExpressionCatalog,
    pub bindings: &'a dyn InferenceBindingScope,
    pub foreign_keys: ForeignKeyDefinitionContext<'a>,
}
fn ddl_storage_error(action: &str, error: ColumnCatalogError) -> SQLError {
    crate::catalog::errors::storage_error(action, error.as_ref())
}
pub fn bind_added_column(
    context: &AddedColumnAnalysisContext<'_>,
    table: &str,
    qualifier: &str,
    column: &mut ColumnDef,
) -> Result<(), SQLError> {
    if let Some(default) = &mut column.default {
        let binding = context.bindings.binding_scope()?;
        crate::schema::defaults::validate_default_expression(
            &SchemaBindingContext {
                catalog: context.schema,
                binding: &binding.context(),
            },
            default,
            &column.ty,
        )?;
    }
    let mut candidate_columns = context
        .foreign_keys
        .columns
        .try_describe_table(table)
        .map_err(|error| ddl_storage_error("ALTER TABLE ADD COLUMN", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    candidate_columns.push(column.clone());
    let check_columns = candidate_columns.clone();
    if let Some(check) = &mut column.check {
        let binding = context.bindings.binding_scope()?;
        crate::schema::constraints::validate_check_expression(
            &SchemaBindingContext {
                catalog: context.schema,
                binding: &binding.context(),
            },
            table,
            qualifier,
            &check_columns,
            check,
        )?;
        crate::catalog::regrole_dependencies::reject_stored_regrole_constants(
            context.schema,
            check,
            None,
        )?;
        candidate_columns
            .last_mut()
            .expect("new column candidate exists")
            .check
            .clone_from(&column.check);
    }
    let key_constraints = context
        .keys
        .try_key_constraints(table)
        .map_err(|error| ddl_storage_error("ALTER TABLE ADD COLUMN", error))?;
    let foreign_keys = context
        .keys
        .try_foreign_keys(table)
        .map_err(|error| ddl_storage_error("ALTER TABLE ADD COLUMN", error))?;
    crate::schema::generated::prepare_generated_columns(
        context.schema,
        qualifier,
        &mut candidate_columns,
        &key_constraints,
        &foreign_keys,
    )?;
    column.generated = candidate_columns
        .last()
        .and_then(|candidate| candidate.generated.clone());
    if let Some(reference) = column.references.clone() {
        let mut foreign_key = crate::schema::foreign_keys::column_foreign_key(column, &reference);
        crate::schema::foreign_keys::validate_foreign_key_definition_with_local_state(
            &context.foreign_keys,
            table,
            Some(&candidate_columns),
            None,
            &mut foreign_key,
        )?;
        let [referenced_column] = foreign_key.ref_columns.as_slice() else {
            return Err(SQLError::Internal(
                "column FOREIGN KEY did not resolve exactly one referenced column".into(),
            ));
        };
        let Some(reference) = column.references.as_mut() else {
            return Err(SQLError::Internal(
                "column FOREIGN KEY disappeared during validation".into(),
            ));
        };
        reference.table = foreign_key.ref_table;
        reference.column = Some(referenced_column.clone());
    }
    Ok(())
}
