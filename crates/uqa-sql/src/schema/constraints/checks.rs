//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! CHECK analysis keeps the declaring relation's actual row namespace and type.

use crate::{
    ast::{ColumnDef, Expr},
    schema::{ColumnIdentity, SchemaBindingContext},
    ColumnType, RowSchema, SQLError,
};
use uqa_core::Value;

/// A CHECK sees user columns, its named whole row and the relation OID; system attributes are not ordinary declaration columns.
pub fn check_row_schema(
    qualifier: &str,
    columns: &[ColumnDef],
    row_type: &ColumnType,
) -> Result<RowSchema, SQLError> {
    let schema = RowSchema::with_column_definitions(Some(qualifier), columns)
        .with_relation_row_type(qualifier, row_type.clone())?;
    Ok(RowSchema::with_typed_virtual_identities(
        &schema,
        &[
            (
                ColumnIdentity::unqualified("tableoid"),
                Some(ColumnType::Oid),
            ),
            (
                ColumnIdentity::qualified(qualifier, "tableoid"),
                Some(ColumnType::Oid),
            ),
        ],
    ))
}

pub fn validate_check_expression(
    context: &SchemaBindingContext<'_, '_>,
    table: &str,
    qualifier: &str,
    columns: &[ColumnDef],
    expression: &mut Expr,
) -> Result<(), SQLError> {
    let row_type = crate::FunctionTypeResolver::resolve_type_name(context.catalog, table)?
        .ok_or_else(|| SQLError::UnknownTable(table.into()))?;
    validate_check_expression_with_type(context, table, qualifier, columns, &row_type, expression)
}

/// Rebind retained calls in the same named row scope used for CHECK analysis, including tableoid and the actual composite identity.
pub fn bind_stored_check_expression(
    context: &SchemaBindingContext<'_, '_>,
    table: &str,
    qualifier: &str,
    columns: &[ColumnDef],
    expression: &mut Expr,
) -> Result<bool, SQLError> {
    let row_type = crate::FunctionTypeResolver::resolve_type_name(context.catalog, table)?
        .ok_or_else(|| SQLError::UnknownTable(table.into()))?;
    let relation = crate::RelationIdentity::from_legacy_name(table).map_err(SQLError::Internal)?;
    let schema = check_row_schema(&relation.name, columns, &row_type)?;
    let typed = super::super::expressions::bind_relation_row_references(
        expression,
        table,
        qualifier,
        &relation.name,
        columns,
    )?;
    crate::schema::defaults::bind_stored_schema_expression_with_schema(
        context, expression, typed, &schema,
    )
}

/// Analyze CHECKs against the actual declared row type, using the same SQL boundary for CREATE and ALTER.
pub fn validate_check_expression_with_type(
    context: &SchemaBindingContext<'_, '_>,
    table: &str,
    qualifier: &str,
    columns: &[ColumnDef],
    row_type: &ColumnType,
    expression: &mut Expr,
) -> Result<(), SQLError> {
    let relation = crate::RelationIdentity::from_legacy_name(table).map_err(SQLError::Internal)?;
    let schema = check_row_schema(&relation.name, columns, row_type)?;
    *expression = super::super::expressions::bind_relation_row_references(
        expression,
        table,
        qualifier,
        &relation.name,
        columns,
    )?;
    bind_whole_row_fields(expression, &schema)?;
    let original = crate::plan::ExpressionPlan::lower(expression.clone());
    let mut plan = original.clone();
    let ty =
        crate::binding::analyze_table_check(context.catalog, &mut plan, &schema, context.binding)?;
    let sites = crate::binding::syntax_sites::expression_syntax_sites(&original, &plan)?;
    crate::catalog::stored_ast::bind_stored_expression_sites(expression, &sites)?;
    require_boolean(context, expression, ty.as_ref())?;
    crate::schema::defaults::bind_stored_schema_expression_with_schema(
        context,
        expression,
        expression.clone(),
        &schema,
    )?;
    crate::schema::generated::bind_schema_column_references(expression, &relation.name);
    crate::schema::check_inheritance::remove_identity_casts(expression, columns)
}

/// Selecting a field of the declaring row denotes that column, not a dependency on every field of a composite value.
fn bind_whole_row_fields(expression: &mut Expr, schema: &RowSchema) -> Result<(), SQLError> {
    crate::catalog::stored_ast::visit_stored_expression(expression, &mut |node| {
        let Expr::Func {
            binding: Some(binding),
            args,
            ..
        } = node
        else {
            return Ok(());
        };
        if binding.dispatch != Some(crate::ast::FunctionDispatch::FieldSelect) {
            return Ok(());
        }
        if let [Expr::QualifiedStar(qualifier), Expr::Literal(Value::Str(column))] = args.as_slice()
        {
            if schema.has_qualifier(qualifier) {
                *node = Expr::QualifiedColumn {
                    qualifier: qualifier.to_string(),
                    column: column.clone().into(),
                };
            }
        }
        Ok(())
    })
}

fn require_boolean(
    context: &SchemaBindingContext<'_, '_>,
    expression: &mut Expr,
    ty: Option<&ColumnType>,
) -> Result<(), SQLError> {
    if let Some(mut ty) = ty {
        while let ColumnType::Domain { base, .. } = ty {
            ty = base;
        }
        if *ty != ColumnType::Boolean {
            return Err(SQLError::Routine {
                sqlstate: "42804".into(),
                message: format!(
                    "argument of CHECK must be type boolean, not type {}",
                    ty.regtype_name()
                ),
            });
        }
    } else if matches!(expression, Expr::Literal(Value::Null | Value::Str(_))) {
        crate::catalog::stored_ast::read_unknown_stored_literal(
            crate::FunctionTypeResolver::enum_labels(context.catalog),
            crate::FunctionTypeResolver::catalog_input_functions(context.catalog),
            expression,
            &ColumnType::Boolean,
            false,
        )?;
    } else {
        *expression = Expr::Cast {
            implicit: true,
            expr: Box::new(expression.clone()),
            ty: "boolean".into(),
        };
    }
    Ok(())
}

#[cfg(test)]
mod tests;
