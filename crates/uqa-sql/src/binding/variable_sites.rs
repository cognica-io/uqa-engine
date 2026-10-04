//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The names in a `PL/pgSQL` statement that the function's variables take, checked against what the statement itself can see, as `PostgreSQL`'s `plpgsql_post_column_ref` checks them: a variable whose name a column or relation of the statement also takes is ambiguous unless `plpgsql.variable_conflict` chooses one of them, and an output column that a bare name in ORDER BY, GROUP BY or DISTINCT ON names takes the name before the parser asks for a variable.

use super::{
    projection_columns, BindingContext, QueryBlockPlan, RowSchema, SQLError, SQLParam, ScalarExpr,
    SchemaScope,
};
use crate::plan::{ProjectionPlan, UnifiedPlan};
use crate::routines::RoutineResolution;

/// How a name that a `PL/pgSQL` variable takes resolves in the statement it appears in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VariableSiteResolution {
    /// Nothing the statement can see there takes the name, so the variable does.
    Variable,
    /// A column or relation the statement can see there takes the name too.
    Column,
    /// An output column takes the name.
    Output,
}

/// The variable sites of the statement a scope binds.
pub(super) struct VariableSites {
    /// The names as written; the site that the positional parameter `n` stands for is at index `n - 1`.
    names: Vec<ScalarExpr>,
    resolutions: Vec<VariableSiteResolution>,
}

impl VariableSites {
    /// The site that `expression` stands for.
    fn site(&self, expression: &ScalarExpr) -> Option<usize> {
        match expression {
            ScalarExpr::Param(number) => number
                .checked_sub(1)
                .filter(|site| *site < self.names.len()),
            _ => None,
        }
    }

    /// The output name a select list item that is the site takes: the name as written.
    fn label(&self, site: usize) -> Option<&str> {
        match &self.names[site] {
            ScalarExpr::Column(name) | ScalarExpr::QualifiedColumn { column: name, .. } => {
                Some(name)
            }
            _ => None,
        }
    }

    /// The site's name when it is written without a qualifier.
    fn bare_name(&self, site: usize) -> Option<&str> {
        match &self.names[site] {
            ScalarExpr::Column(name) => Some(name),
            _ => None,
        }
    }
}

/// Whether a column or relation of `schema` takes `name`.
fn takes(schema: &RowSchema, name: &ScalarExpr) -> bool {
    match name {
        ScalarExpr::Column(name) => {
            schema.has_unqualified_column(name)
                || schema.column_is_ambiguous(name)
                || schema.has_qualifier(name)
        }
        ScalarExpr::QualifiedColumn { qualifier, column } => {
            schema.has_qualified_column(qualifier, column)
                || schema.qualified_column_is_ambiguous(qualifier, column)
        }
        _ => false,
    }
}

impl SchemaScope {
    /// Record each variable site in `expression` that a column or relation of `schema` also takes, and let the column take it for the rest of the analysis, as the parser's own resolution does before its hook reports the conflict.
    pub(super) fn resolve_variable_sites(
        &mut self,
        expression: &mut ScalarExpr,
        schema: &RowSchema,
    ) {
        let Some(sites) = self.variable_sites.as_mut() else {
            return;
        };
        crate::plan::rewrite_scalar_expression(expression, &mut |node| {
            if let Some(site) = sites.site(node) {
                if sites.resolutions[site] == VariableSiteResolution::Variable
                    && takes(schema, &sites.names[site])
                {
                    sites.resolutions[site] = VariableSiteResolution::Column;
                    node.clone_from(&sites.names[site]);
                }
            }
        });
    }

    /// The output names of a select list, a variable site taking the name it is written with.
    pub(super) fn output_names(&self, projections: &[ProjectionPlan]) -> Vec<String> {
        let mut names = projection_columns(projections);
        if let Some(sites) = self.variable_sites.as_ref() {
            for (name, projection) in names.iter_mut().zip(projections) {
                if projection.alias.is_some() {
                    continue;
                }
                if let Some(label) = sites
                    .site(&projection.expr)
                    .and_then(|site| sites.label(site))
                {
                    label.clone_into(name);
                }
            }
        }
        names
    }

    /// Whether `expression`, an ORDER BY or DISTINCT ON item, is a variable site whose bare name an output column takes, as `findTargetlistEntrySQL92` matches it before anything else; records the site.
    pub(super) fn output_takes_variable_site(
        &mut self,
        expression: &ScalarExpr,
        output_names: &[String],
    ) -> bool {
        let Some(sites) = self.variable_sites.as_mut() else {
            return false;
        };
        let Some(site) = sites.site(expression) else {
            return false;
        };
        if !sites
            .bare_name(site)
            .is_some_and(|name| output_names.iter().any(|output| output == name))
        {
            return false;
        }
        sites.resolutions[site] = VariableSiteResolution::Output;
        true
    }

    /// Replace each GROUP BY item that is a variable site whose bare name no column of the query's own sources has but an output column does with that output column's expression, as `findTargetlistEntrySQL92` resolves it; records the site.
    pub(super) fn bind_grouping_variable_sites(
        &mut self,
        block: &mut QueryBlockPlan,
        source: &RowSchema,
    ) {
        let output_names = self.output_names(&block.projections);
        let Some(sites) = self.variable_sites.as_mut() else {
            return;
        };
        for item in block
            .group_by
            .iter_mut()
            .chain(block.grouping_sets.iter_mut().flatten())
        {
            let Some(site) = sites.site(item) else {
                continue;
            };
            let Some(name) = sites.bare_name(site) else {
                continue;
            };
            if source.has_unqualified_column(name) || source.column_is_ambiguous(name) {
                continue;
            }
            let Some(position) = output_names.iter().position(|output| output == name) else {
                continue;
            };
            sites.resolutions[site] = VariableSiteResolution::Output;
            item.clone_from(&block.projections[position].expr);
        }
    }
}

/// Find how each variable site of a `PL/pgSQL` statement resolves in the statement. `plan` is the statement with each name that the function's variables take replaced by the positional parameter whose number is the site's, typed by `params`, and `names` holds those names as written, in site order.
pub fn resolve_variable_sites(
    routines: &dyn RoutineResolution,
    plan: &mut UnifiedPlan,
    params: &[SQLParam],
    ctes: &BindingContext,
    names: Vec<ScalarExpr>,
) -> Result<Vec<VariableSiteResolution>, SQLError> {
    let mut scope = SchemaScope::for_analysis(ctes)?;
    scope.binds_routine_identities = false;
    scope.variable_sites = Some(VariableSites {
        resolutions: vec![VariableSiteResolution::Variable; names.len()],
        names,
    });
    let walked = scope.bind_statement_parameters(routines, plan, params, None);
    let resolutions = scope
        .variable_sites
        .take()
        .map(|sites| sites.resolutions)
        .unwrap_or_default();
    match walked {
        Ok(()) => Ok(resolutions),
        // The walk meets a name in the order the parser analyzes it, so a conflict it met before an error that analysis finds later, such as an ungrouped column, is what the parser reports.
        Err(_) if resolutions.contains(&VariableSiteResolution::Column) => Ok(resolutions),
        Err(error) => Err(error),
    }
}
