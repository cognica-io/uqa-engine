//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Variable references of embedded statements resolved as `PostgreSQL`'s parser hooks for `PL/pgSQL` resolve them: a name that a variable takes is checked against the columns and relations the statement can see there, and a name that both take is ambiguous unless `plpgsql.variable_conflict` chooses one.
//!
//! Binding a statement replaces each variable reference with the variable's value. To learn how each reference resolves, the statement is first bound with every reference replaced by a positional parameter numbered after the reference, which the binder resolves in the statement's own scopes, and then bound again leaving to the statement each name that resolves elsewhere. Both bindings walk the statement in the same order, so the numbers identify the references without relating the statement's syntax to its lowered plan.

use super::binding::{bind_expr, bind_statement, ResolvedVariable, VariableResolver};
use super::options::VariableConflict;
use super::{Expr, Projection, Result, SelectStmt, Statement};
use crate::ast::{ColumnType, InternalColumnRef};
use crate::binding::VariableSiteResolution;
use crate::{SQLError, SQLParam, ScalarExpr};
use uqa_core::Value;

/// A variable reference of a statement: the name as written, and the value the function's resolver binds it to.
struct VariableSite {
    reference: Expr,
    binding: Expr,
}

/// Resolve how the variable sites of a statement resolve in it: the statement with each site as the positional parameter numbered after it, the sites' values as typed parameters, and the sites' names as written.
pub type VariableSiteResolver<'a> =
    &'a mut dyn FnMut(
        Statement,
        Vec<SQLParam>,
        Vec<ScalarExpr>,
    ) -> std::result::Result<Vec<VariableSiteResolution>, SQLError>;

/// Number each variable reference the inner resolver binds and stand a positional parameter in for it.
struct VariableSiteMarker<'a> {
    inner: &'a mut dyn VariableResolver,
    sites: Vec<VariableSite>,
}

impl VariableSiteMarker<'_> {
    fn mark(&mut self, reference: Expr, binding: Option<Expr>) -> Option<Expr> {
        let binding = binding?;
        self.sites.push(VariableSite { reference, binding });
        Some(Expr::Param(self.sites.len()))
    }
}

impl VariableResolver for VariableSiteMarker<'_> {
    fn resolve_name(&mut self, name: &str) -> Result<Option<ResolvedVariable>> {
        self.inner.resolve_name(name)
    }

    fn resolve_qualified(
        &mut self,
        qualifier: &str,
        column: &str,
    ) -> Result<Option<ResolvedVariable>> {
        self.inner.resolve_qualified(qualifier, column)
    }

    fn resolve_param(&mut self, index: usize) -> Result<Option<ResolvedVariable>> {
        self.inner.resolve_param(index)
    }

    fn rewrite_name(&mut self, name: &str) -> Result<Option<Expr>> {
        let binding = self.inner.rewrite_name(name)?;
        Ok(self.mark(Expr::Column(name.to_string()), binding))
    }

    fn rewrite_qualified(&mut self, qualifier: &str, column: &str) -> Result<Option<Expr>> {
        let binding = self.inner.rewrite_qualified(qualifier, column)?;
        Ok(self.mark(
            Expr::QualifiedColumn {
                qualifier: qualifier.to_string(),
                column: column.to_string(),
            },
            binding,
        ))
    }

    fn rewrite_qualified_star(&mut self, qualifier: &str) -> Result<Option<Vec<Expr>>> {
        self.inner.rewrite_qualified_star(qualifier)
    }

    fn rewrite_qualified_whole_row(&mut self, qualifier: &str) -> Result<Option<Expr>> {
        self.inner.rewrite_qualified_whole_row(qualifier)
    }

    fn rewrite_param(&mut self, index: usize) -> Result<Option<Expr>> {
        self.inner.rewrite_param(index)
    }

    fn rewrite_internal(&mut self, column: InternalColumnRef) -> Result<Option<Expr>> {
        self.inner.rewrite_internal(column)
    }
}

/// Bind the variable references the inner resolver binds, except those whose site `keep` marks, whose names the statement resolves.
struct VariableSiteFilter<'a> {
    inner: &'a mut dyn VariableResolver,
    keep: &'a [bool],
    next: usize,
}

impl VariableSiteFilter<'_> {
    fn filter(&mut self, binding: Option<Expr>) -> Option<Expr> {
        let binding = binding?;
        let site = self.next;
        self.next += 1;
        (!self.keep.get(site).copied().unwrap_or(false)).then_some(binding)
    }
}

impl VariableResolver for VariableSiteFilter<'_> {
    fn resolve_name(&mut self, name: &str) -> Result<Option<ResolvedVariable>> {
        self.inner.resolve_name(name)
    }

    fn resolve_qualified(
        &mut self,
        qualifier: &str,
        column: &str,
    ) -> Result<Option<ResolvedVariable>> {
        self.inner.resolve_qualified(qualifier, column)
    }

    fn resolve_param(&mut self, index: usize) -> Result<Option<ResolvedVariable>> {
        self.inner.resolve_param(index)
    }

    fn rewrite_name(&mut self, name: &str) -> Result<Option<Expr>> {
        let binding = self.inner.rewrite_name(name)?;
        Ok(self.filter(binding))
    }

    fn rewrite_qualified(&mut self, qualifier: &str, column: &str) -> Result<Option<Expr>> {
        let binding = self.inner.rewrite_qualified(qualifier, column)?;
        Ok(self.filter(binding))
    }

    fn rewrite_qualified_star(&mut self, qualifier: &str) -> Result<Option<Vec<Expr>>> {
        self.inner.rewrite_qualified_star(qualifier)
    }

    fn rewrite_qualified_whole_row(&mut self, qualifier: &str) -> Result<Option<Expr>> {
        self.inner.rewrite_qualified_whole_row(qualifier)
    }

    fn rewrite_param(&mut self, index: usize) -> Result<Option<Expr>> {
        self.inner.rewrite_param(index)
    }

    fn rewrite_internal(&mut self, column: InternalColumnRef) -> Result<Option<Expr>> {
        self.inner.rewrite_internal(column)
    }
}

/// The error `plpgsql_post_column_ref` raises for a name that a variable and a column or relation both take under `plpgsql.variable_conflict = error`.
fn ambiguous_variable_error(reference: &Expr) -> SQLError {
    let name = match reference {
        Expr::QualifiedColumn { qualifier, column } => format!("{qualifier}.{column}"),
        Expr::Column(name) => name.clone(),
        other => format!("{other:?}"),
    };
    SQLError::Diagnostic {
        sqlstate: "42702".into(),
        message: format!("column reference \"{name}\" is ambiguous"),
        detail: Some("It could refer to either a PL/pgSQL variable or a table column.".into()),
        hint: None,
    }
}

/// The typed parameter that stands for a site's value while the binder resolves the statement.
fn site_parameter(binding: &Expr) -> SQLParam {
    match binding {
        Expr::TypedLiteral { value, ty } => ColumnType::from_sql_name(ty).map_or_else(
            |_| SQLParam::scalar(value.clone()),
            |ty| SQLParam::typed_scalar(value.clone(), ty),
        ),
        Expr::Literal(value) => SQLParam::scalar(value.clone()),
        _ => SQLParam::scalar(Value::Null),
    }
}

/// The name a site is written with, as the binder reads it.
fn site_name(reference: &Expr) -> ScalarExpr {
    match reference {
        Expr::QualifiedColumn { qualifier, column } => ScalarExpr::QualifiedColumn {
            qualifier: qualifier.clone(),
            column: column.clone(),
        },
        Expr::Column(name) => ScalarExpr::Column(name.clone()),
        _ => ScalarExpr::Literal(Value::Null),
    }
}

/// Which sites keep their names: those an output column takes, and those a column takes when `conflict` chooses the column. A site a column takes under `error` fails the statement.
fn kept_names(
    sites: &[VariableSite],
    resolutions: &[VariableSiteResolution],
    conflict: VariableConflict,
) -> std::result::Result<Vec<bool>, SQLError> {
    sites
        .iter()
        .zip(resolutions)
        .map(|(site, resolution)| match (resolution, conflict) {
            (VariableSiteResolution::Output, _)
            | (VariableSiteResolution::Column, VariableConflict::UseColumn) => Ok(true),
            (VariableSiteResolution::Column, VariableConflict::Error) => {
                Err(ambiguous_variable_error(&site.reference))
            }
            (VariableSiteResolution::Column, VariableConflict::UseVariable)
            | (VariableSiteResolution::Variable, _) => Ok(false),
        })
        .collect()
}

/// Bind the variables of an embedded statement, resolving each name that a variable takes against the columns and relations the statement can see as `conflict` directs.
pub fn bind_statement_variables(
    statement: &Statement,
    resolver: &mut dyn VariableResolver,
    conflict: VariableConflict,
    resolve_sites: VariableSiteResolver<'_>,
) -> Result<Statement> {
    let mut numbering = VariableSiteMarker {
        inner: resolver,
        sites: Vec::new(),
    };
    let numbered = bind_statement(statement, &mut numbering)?;
    let sites = numbering.sites;
    if sites.is_empty() {
        return Ok(numbered);
    }
    let resolutions = resolve_sites(
        numbered,
        sites
            .iter()
            .map(|site| site_parameter(&site.binding))
            .collect(),
        sites
            .iter()
            .map(|site| site_name(&site.reference))
            .collect(),
    )?;
    let keep = kept_names(&sites, &resolutions, conflict)?;
    bind_statement(
        statement,
        &mut VariableSiteFilter {
            inner: resolver,
            keep: &keep,
            next: 0,
        },
    )
}

/// Bind the variables of an embedded expression, which `PostgreSQL` analyzes as `SELECT expression`: only a subquery lets a name of the expression meet a column, so an expression without one binds every variable it names.
pub fn bind_expression_variables(
    expression: &Expr,
    resolver: &mut dyn VariableResolver,
    conflict: VariableConflict,
    resolve_sites: VariableSiteResolver<'_>,
) -> Result<Expr> {
    let queries = expression.any_node(&|node| {
        matches!(
            node,
            Expr::ScalarSubquery(_) | Expr::Exists { .. } | Expr::InSubquery { .. }
        )
    });
    if !queries {
        return bind_expr(expression, resolver);
    }
    let mut numbering = VariableSiteMarker {
        inner: resolver,
        sites: Vec::new(),
    };
    let numbered = bind_expr(expression, &mut numbering)?;
    let sites = numbering.sites;
    if sites.is_empty() {
        return Ok(numbered);
    }
    let resolutions = resolve_sites(
        expression_query(numbered),
        sites
            .iter()
            .map(|site| site_parameter(&site.binding))
            .collect(),
        sites
            .iter()
            .map(|site| site_name(&site.reference))
            .collect(),
    )?;
    let keep = kept_names(&sites, &resolutions, conflict)?;
    bind_expr(
        expression,
        &mut VariableSiteFilter {
            inner: resolver,
            keep: &keep,
            next: 0,
        },
    )
}

/// `SELECT expression`, the query `PostgreSQL` analyzes for an embedded expression.
fn expression_query(expression: Expr) -> Statement {
    Statement::Select(Box::new(SelectStmt {
        projections: vec![Projection {
            expr: expression,
            alias: None,
        }],
        values: Vec::new(),
        from: None,
        r#where: None,
        group_by: Vec::new(),
        grouping_sets: Vec::new(),
        group_distinct: false,
        having: None,
        order_by: Vec::new(),
        limit: None,
        with_ties: false,
        offset: None,
        with: Vec::new(),
        set_op: None,
        distinct: false,
        distinct_on: Vec::new(),
        locking: Vec::new(),
    }))
}
