//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared stored-expression analysis. SQL retains the original bound syntax; a planner-owned copy decides immutability and constant partition keys.

use super::SchemaBindingContext;
use crate::{
    ast::{ColumnDef, Expr},
    plan::ExpressionPlan,
    ColumnType, RowSchema, SQLError, ScalarExpr,
};

pub struct AnalyzedSchemaExpression {
    pub expression: Expr,
    pub ty: Option<ColumnType>,
    pub scalar: ScalarExpr,
    pub schema: RowSchema,
}

pub struct PlannedSchemaExpression {
    pub expression: Expr,
    pub ty: Option<ColumnType>,
    pub immutable: bool,
    pub constant: bool,
}

pub fn row_schema(columns: &[ColumnDef]) -> RowSchema {
    RowSchema::with_types(
        columns.iter().map(|column| column.name.clone()).collect(),
        columns
            .iter()
            .map(|column| Some(column.ty.clone()))
            .collect(),
    )
}

/// Analyze every branch before planning, and bind original routine/type/input identities for storage. Assignment coercion follows the planner's mutability check.
pub fn analyze_schema_expression(
    context: &SchemaBindingContext<'_, '_>,
    expression: &Expr,
    columns: &[ColumnDef],
) -> Result<AnalyzedSchemaExpression, SQLError> {
    let mut expression = expression.clone();
    let schema = row_schema(columns);
    let original = ExpressionPlan::lower(expression.clone());
    let mut analyzed = original.clone();
    let source = crate::binding::analyze_stored_expression_inputs(
        context.catalog,
        &mut analyzed,
        context.binding,
        &schema,
    )?;
    crate::binding::bind_expression_plan_routines_for_storage(
        context.catalog,
        &mut analyzed,
        &[],
        context.binding,
        &schema,
    )?;
    let sites = crate::binding::syntax_sites::expression_syntax_sites(&original, &analyzed)?;
    crate::catalog::stored_ast::bind_stored_expression_sites(&mut expression, &sites)?;
    super::dependencies::oid_alias::read_oid_alias_constants(context.catalog, &mut expression)?;
    let scalar = crate::type_resolution::bind_type_introspection_with_resolver(
        analyzed.scalar,
        &schema,
        &[],
        context.catalog,
    );
    Ok(AnalyzedSchemaExpression {
        expression,
        ty: source,
        scalar,
        schema,
    })
}
