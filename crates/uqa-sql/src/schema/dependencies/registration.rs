//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind and rewrite durable routine identities in stored SQL schema expressions.
use super::{regclass::SchemaReferenceCatalog, walk_schema_expr_mut};
use crate::schema::{SchemaBindingContext, SchemaExpressionCatalog};
use crate::semantics::conflict::InferenceBindingScope;
use crate::{
    ast::{ColumnDef, Expr},
    SQLError,
};

pub struct SchemaDependencyBindingContext<'a> {
    pub references: &'a dyn SchemaReferenceCatalog,
    pub schema: &'a dyn SchemaExpressionCatalog,
    pub bindings: &'a dyn InferenceBindingScope,
}

pub fn rewrite_schema_routine_references(
    columns: &mut [crate::ast::ColumnDef],
    checks: &mut [crate::ast::TableCheck],
    target: &crate::ast::FunctionBinding,
    new_name: &str,
) -> Result<bool, String> {
    let mut changed = false;
    for column in columns {
        for expression in [&mut column.default, &mut column.check]
            .into_iter()
            .flatten()
        {
            changed |= crate::catalog::stored_ast::rewrite_expression_routine_identity(
                expression, target, new_name,
            )
            .map_err(|error| error.to_string())?;
        }
        if let Some(generated) = column.generated.as_mut() {
            changed |= crate::catalog::stored_ast::rewrite_expression_routine_identity(
                &mut generated.expression,
                target,
                new_name,
            )
            .map_err(|error| error.to_string())?;
            for dependency in &mut generated.function_dependencies {
                if crate::routines::function_binding_matches(dependency, target) {
                    dependency.name = new_name.to_string();
                    changed = true;
                }
            }
        }
    }
    for check in checks {
        changed |= crate::catalog::stored_ast::rewrite_expression_routine_identity(
            &mut check.expr,
            target,
            new_name,
        )
        .map_err(|error| error.to_string())?;
    }
    Ok(changed)
}

fn schema_expr_may_require_routine_identity_binding(
    expression: &crate::ast::Expr,
) -> Result<bool, String> {
    let mut expression = expression.clone();
    let mut legacy = false;
    walk_schema_expr_mut(&mut expression, &mut |node| {
        if let crate::ast::Expr::Func { binding, .. } = node {
            legacy |= binding.as_ref().is_none_or(|binding| {
                !binding.builtin
                    && binding.dispatch.is_none()
                    && binding.resolution_error.is_none()
                    && binding.object_id.is_none()
            });
        }
        Ok(())
    })?;
    Ok(legacy)
}

pub fn schema_expr_has_legacy_routine_identity(
    expression: &crate::ast::Expr,
) -> Result<bool, String> {
    let mut expression = expression.clone();
    let mut legacy = false;
    walk_schema_expr_mut(&mut expression, &mut |node| {
        if let crate::ast::Expr::Func {
            binding: Some(binding),
            ..
        } = node
        {
            legacy |= !binding.builtin
                && binding.dispatch.is_none()
                && binding.resolution_error.is_none()
                && binding.object_id.is_none();
        }
        Ok(())
    })?;
    Ok(legacy)
}

pub fn bind_table_schema_routine_identities(
    context: &SchemaDependencyBindingContext<'_>,
    table_name: &str,
    columns: &mut [crate::ast::ColumnDef],
    checks: &mut [crate::ast::TableCheck],
) -> Result<bool, String> {
    let check_columns = columns.to_vec();
    bind_table_schema_routine_identities_with_check_columns(
        context,
        table_name,
        columns,
        checks,
        &check_columns,
    )
}

pub fn bind_table_schema_routine_identities_with_check_columns(
    context: &SchemaDependencyBindingContext<'_>,
    table_name: &str,
    columns: &mut [crate::ast::ColumnDef],
    checks: &mut [crate::ast::TableCheck],
    check_columns: &[crate::ast::ColumnDef],
) -> Result<bool, String> {
    let mut changed = super::regclass::bind_table_schema_regclass_constants(
        context.references,
        columns,
        checks,
        false,
    )?;
    for column in columns {
        if let Some(default) = &mut column.default {
            changed |= bind_default_routine_identities(context, table_name, &column.name, default)?;
        }
        if let Some(check) = &mut column.check {
            if schema_expr_may_require_routine_identity_binding(check)? {
                changed |=
                    bind_check_routines(context, table_name, table_name, check_columns, check)
                        .map_err(|error| {
                            format!(
                                "bind CHECK routine identities for `{table_name}`.`{}`: {error}",
                                column.name
                            )
                        })?;
            }
        }
    }
    for check in checks {
        if schema_expr_may_require_routine_identity_binding(&check.expr)? {
            changed |= bind_check_routines(
                context,
                table_name,
                table_name,
                check_columns,
                &mut check.expr,
            )
            .map_err(|error| {
                format!("bind CHECK routine identities for `{table_name}`: {error}")
            })?;
        }
    }
    Ok(changed)
}

pub fn bind_default_routine_identities(
    context: &SchemaDependencyBindingContext<'_>,
    table_name: &str,
    column_name: &str,
    default: &mut crate::ast::Expr,
) -> Result<bool, String> {
    let changed =
        super::regclass::bind_schema_regclass_constants(context.references, default, false)?;
    if !schema_expr_may_require_routine_identity_binding(default)? {
        return Ok(changed);
    }
    let bound = bind_default_routines(context, default, default.clone()).map_err(|error| {
        format!("bind default routine identities for `{table_name}`.`{column_name}`: {error}")
    })?;
    Ok(changed || bound)
}

fn bind_check_routines(
    context: &SchemaDependencyBindingContext<'_>,
    table: &str,
    qualifier: &str,
    columns: &[ColumnDef],
    expression: &mut Expr,
) -> Result<bool, SQLError> {
    let binding = context.bindings.binding_scope()?;
    crate::schema::constraints::bind_stored_check_expression_routines(
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
fn bind_default_routines(
    context: &SchemaDependencyBindingContext<'_>,
    expression: &mut Expr,
    typed: Expr,
) -> Result<bool, SQLError> {
    let binding = context.bindings.binding_scope()?;
    crate::schema::defaults::bind_stored_schema_expression_routines(
        &SchemaBindingContext {
            catalog: context.schema,
            binding: &binding.context(),
        },
        expression,
        typed,
    )
}
