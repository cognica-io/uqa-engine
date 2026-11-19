//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared stored-expression analysis. SQL retains the original bound syntax; a planner-owned copy decides immutability and constant partition keys.

use super::SchemaBindingContext;
use crate::plpgsql::{ResolvedVariable, VariableResolver};
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

/// Bind a declaring relation's whole row while preserving column-name precedence.
pub(super) fn bind_relation_row_references(
    expression: &Expr,
    table: &str,
    qualifier: &str,
    local: &str,
    columns: &[ColumnDef],
) -> Result<Expr, SQLError> {
    crate::plpgsql::bind_expr(
        expression,
        &mut RelationReferences {
            table,
            qualifier,
            local,
            columns,
        },
    )
}

struct RelationReferences<'a> {
    table: &'a str,
    qualifier: &'a str,
    local: &'a str,
    columns: &'a [ColumnDef],
}

impl RelationReferences<'_> {
    fn matches(&self, qualifier: &str) -> bool {
        [self.table, self.qualifier, self.local].contains(&qualifier)
    }
}

impl VariableResolver for RelationReferences<'_> {
    fn resolve_name(&mut self, _: &str) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }

    fn resolve_qualified(
        &mut self,
        _: &str,
        _: &str,
    ) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }

    fn resolve_param(&mut self, _: usize) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }

    fn rewrite_name(&mut self, name: &str) -> Result<Option<Expr>, SQLError> {
        Ok(
            (self.matches(name) && !self.columns.iter().any(|column| column.name == name))
                .then(|| Expr::QualifiedStar(self.local.into())),
        )
    }

    fn rewrite_qualified(
        &mut self,
        qualifier: &str,
        column: &str,
    ) -> Result<Option<Expr>, SQLError> {
        Ok(self.matches(qualifier).then(|| Expr::QualifiedColumn {
            qualifier: self.local.into(),
            column: column.into(),
        }))
    }

    fn rewrite_qualified_whole_row(&mut self, qualifier: &str) -> Result<Option<Expr>, SQLError> {
        Ok(self
            .matches(qualifier)
            .then(|| Expr::QualifiedStar(self.local.into())))
    }
}

pub struct PlannedSchemaExpression {
    pub expression: Expr,
    pub ty: Option<ColumnType>,
    pub immutable: bool,
    pub constant: bool,
}

/// The declared row of a schema expression, with each column's collation, which the input collations of its calls derive from.
pub fn row_schema(columns: &[ColumnDef]) -> RowSchema {
    RowSchema::with_column_definitions(None, columns)
}

/// Analyze every branch before planning, and bind original routine/type/input identities for storage. Assignment coercion follows the planner's mutability check.
pub fn analyze_schema_expression(
    context: &SchemaBindingContext<'_, '_>,
    expression: &Expr,
    schema: &RowSchema,
) -> Result<AnalyzedSchemaExpression, SQLError> {
    let mut expression = expression.clone();
    let original = ExpressionPlan::lower(expression.clone());
    let mut analyzed = original.clone();
    let source = crate::binding::analyze_stored_expression_inputs(
        context.catalog,
        &mut analyzed,
        context.binding,
        schema,
    )?;
    crate::binding::bind_expression_plan_routines_for_storage(
        context.catalog,
        &mut analyzed,
        &[],
        context.binding,
        schema,
    )?;
    let sites = crate::binding::syntax_sites::expression_syntax_sites(&original, &analyzed)?;
    crate::catalog::stored_ast::bind_stored_expression_sites(&mut expression, &sites)?;
    super::dependencies::oid_alias::read_oid_alias_constants(context.catalog, &mut expression)?;
    let scalar = crate::type_resolution::bind_type_introspection_with_resolver(
        analyzed.scalar,
        schema,
        &[],
        context.catalog,
    );
    Ok(AnalyzedSchemaExpression {
        expression,
        ty: source,
        scalar,
        schema: schema.clone(),
    })
}
