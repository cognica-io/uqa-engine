//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Foreign-table declaration validation over fresh schema binding scopes and loaded sequence names.
use crate::schema::constraint_metadata::ConstraintMetadataResult;
use crate::schema::dependencies::regclass::SchemaReferenceCatalog;
use crate::schema::sequences::implicit_ownership::StoredSequenceNames;
use crate::schema::{SchemaBindingContext, SchemaExpressionCatalog};
use crate::semantics::conflict::InferenceBindingScope;
use crate::type_resolution::FunctionTypeResolver;
use crate::{
    ast::{ColumnDef, ColumnType, Expr, TableCheck},
    SQLError,
};
use uqa_core::RelationIdentity;

pub struct ForeignSchemaContext<'a> {
    pub types: &'a dyn FunctionTypeResolver,
    pub schema: &'a dyn SchemaExpressionCatalog,
    pub bindings: &'a dyn InferenceBindingScope,
    pub references: &'a dyn SchemaReferenceCatalog,
    pub sequences: &'a dyn StoredSequenceNames,
    pub allocate_identity: fn(&str) -> ConstraintMetadataResult<[u8; 16]>,
}

pub fn validate_foreign_table_schema_envelope(columns: &[ColumnDef]) -> Result<(), SQLError> {
    let mut names = std::collections::BTreeSet::new();
    for column in columns {
        if !names.insert(column.name.as_str()) {
            return Err(SQLError::Routine {
                sqlstate: "42701".into(),
                message: format!("column \"{}\" specified more than once", column.name),
            });
        }
        crate::schema::columns::validate_postgres_column_name(&column.name)?;
        crate::schema::columns::validate_postgres_relation_column_type(&column.name, &column.ty)?;
        if column.primary_key || column.unique {
            let kind = if column.primary_key {
                "primary key"
            } else {
                "unique"
            };
            return Err(SQLError::Unsupported(format!(
                "{kind} constraints are not supported on foreign tables"
            )));
        }
        if column.references.is_some() {
            return Err(SQLError::Unsupported(
                "foreign key constraints are not supported on foreign tables".into(),
            ));
        }
    }
    Ok(())
}
impl ForeignSchemaContext<'_> {
    pub fn prepare_foreign_table_schema(
        &self,
        table_name: &str,
        columns: &mut [ColumnDef],
        checks: &mut Vec<TableCheck>,
    ) -> Result<(), SQLError> {
        self.prepare_foreign_table_schema_inner(table_name, columns, checks, false)
    }
    pub fn prepare_stored_foreign_table_schema(
        &self,
        table_name: &str,
        columns: &mut [ColumnDef],
        checks: &mut Vec<TableCheck>,
    ) -> Result<(), SQLError> {
        validate_foreign_table_schema_envelope(columns)?;
        self.prepare_foreign_table_schema_inner(table_name, columns, checks, true)
    }
    fn prepare_foreign_table_schema_inner(
        &self,
        table_name: &str,
        columns: &mut [ColumnDef],
        checks: &mut Vec<TableCheck>,
        stored: bool,
    ) -> Result<(), SQLError> {
        let relation = RelationIdentity::from_legacy_name(table_name).map_err(|error| {
            SQLError::Internal(format!("decode foreign table `{table_name}`: {error}"))
        })?;
        let qualifier = relation.name.clone();
        let check_columns = columns.to_vec();
        for column in columns.iter_mut() {
            if let Some(default) = &mut column.default {
                prepare_foreign_table_sequence_references(
                    self.references,
                    self.sequences,
                    default,
                    stored,
                )?;
                validate_default_expression(self, default, &column.ty)?;
            }
            if let Some(check) = &mut column.check {
                prepare_foreign_table_sequence_references(
                    self.references,
                    self.sequences,
                    check,
                    stored,
                )?;
                validate_check_expression(self, table_name, &qualifier, &check_columns, check)?;
                crate::catalog::regrole_dependencies::reject_stored_regrole_constants(
                    self.schema,
                    check,
                    None,
                )?;
            }
            if let Some(generated) = &mut column.generated {
                prepare_foreign_table_sequence_references(
                    self.references,
                    self.sequences,
                    &mut generated.expression,
                    stored,
                )?;
            }
        }
        for check in checks.iter_mut() {
            prepare_foreign_table_sequence_references(
                self.references,
                self.sequences,
                &mut check.expr,
                stored,
            )?;
            validate_check_expression(
                self,
                table_name,
                &qualifier,
                &check_columns,
                &mut check.expr,
            )?;
            crate::catalog::regrole_dependencies::reject_stored_regrole_constants(
                self.schema,
                &check.expr,
                None,
            )?;
        }
        crate::schema::generated::prepare_generated_columns(
            self.schema,
            &qualifier,
            columns,
            &[],
            &[],
        )?;
        let mut constraints = crate::ast::TableConstraintSet {
            checks: std::mem::take(checks),
            ..crate::ast::TableConstraintSet::default()
        };
        let mut allocate = self.allocate_identity;
        crate::schema::constraint_metadata::materialize_constraint_metadata(
            &relation,
            columns,
            &mut constraints,
            &mut allocate,
        )
        .map_err(|error| SQLError::Internal(error.to_string()))?;
        *checks = constraints.checks;
        Ok(())
    }
}
fn validate_default_expression(
    context: &ForeignSchemaContext<'_>,
    expression: &mut Expr,
    target: &ColumnType,
) -> Result<(), SQLError> {
    let binding = context.bindings.binding_scope()?;
    crate::schema::defaults::validate_default_expression(
        &SchemaBindingContext {
            catalog: context.schema,
            binding: &binding.context(),
        },
        expression,
        target,
    )
}
fn validate_check_expression(
    context: &ForeignSchemaContext<'_>,
    table: &str,
    qualifier: &str,
    columns: &[ColumnDef],
    expression: &mut Expr,
) -> Result<(), SQLError> {
    let binding = context.bindings.binding_scope()?;
    crate::schema::constraints::validate_check_expression(
        &SchemaBindingContext {
            catalog: context.schema,
            binding: &binding.context(),
        },
        table,
        qualifier,
        columns,
        expression,
    )
}

fn prepare_foreign_table_sequence_references(
    references: &dyn SchemaReferenceCatalog,
    sequences: &dyn StoredSequenceNames,
    expression: &mut crate::ast::Expr,
    stored: bool,
) -> Result<(), SQLError> {
    crate::schema::dependencies::regclass::bind_schema_regclass_constants(
        references, expression, stored,
    )
    .map_err(|error| SQLError::Internal(error.to_string()))?;
    let result = if stored {
        crate::schema::dependencies::rewrites::rewrite_sequence_function_references(
            expression,
            &mut |reference| {
                *reference = sequences.stored_sequence_name(reference)?;
                Ok(())
            },
        )
    } else {
        crate::schema::dependencies::regclass::bind_sequence_references_in_expr(
            references, expression,
        )
    };
    result.map_err(|error| SQLError::Internal(error.to_string()))
}

#[cfg(test)]
mod tests;
