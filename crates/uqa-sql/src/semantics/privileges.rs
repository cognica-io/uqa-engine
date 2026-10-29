//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Semantic base-column privilege analysis for query sources.

pub mod columns;
pub mod coverage;
mod enforcement;

use crate::catalog::roles::RoleReference;
use std::collections::BTreeSet;

use crate::plan::{QueryBlockPlan, QueryPlan, RelationalPlan, SourcePlan};
use crate::SQLError;
use crate::ScalarExpr;

pub mod context;
use context::PrivilegeScope as CteScope;
use enforcement::ensure_required_select;

mod lineage;
mod ownership;
#[cfg(test)]
mod ownership_tests;
use lineage::{
    function_lineage, join_lineage, opaque_columns, opaque_lineage, query_columns,
    relation_lineage, table_lineage, values_lineage,
};
use ownership::{insert_set, require, scopes, Required, SourceLineage};
use uqa_core::memory::{OwnedSet, ProductionControl};

fn is_system_column(column: &str) -> bool {
    matches!(
        column,
        "ctid" | "xmin" | "cmin" | "xmax" | "cmax" | "tableoid"
    )
}

fn collect_column(
    qualifier: Option<&str>,
    column: &str,
    scopes: &[&SourceLineage],
    required: &mut Required,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    if let Some(qualifier) = qualifier {
        if let Some(scope) = scopes.iter().find(|scope| scope.has_qualifier(qualifier)) {
            scope.require_qualified(qualifier, column, required, control)?;
            if is_system_column(column) {
                scope.require_qualified_system(qualifier, column, required, control)?;
            }
        }
    } else if let Some(scope) = scopes.iter().find(|scope| scope.has_unqualified(column)) {
        scope.require_unqualified(column, required, control)?;
    } else if is_system_column(column) {
        if let Some(scope) = scopes
            .iter()
            .find(|scope| !scope.system_qualifiers.is_empty())
        {
            scope.require_unqualified_system(column, required, control)?;
        }
    }
    Ok(())
}

fn collect_expression_columns(
    expression: &ScalarExpr,
    scopes: &[&SourceLineage],
    direct_star: bool,
    required: &mut Required,
    control: &ProductionControl<'_>,
) -> Result<OwnedSet<usize>, SQLError> {
    control.check()?;
    if direct_star {
        match expression {
            ScalarExpr::Star => {
                if let Some(scope) = scopes.iter().find(|scope| !scope.output.is_empty()) {
                    scope.require_all(required, control)?;
                }
                return Ok(OwnedSet::new());
            }
            ScalarExpr::QualifiedStar(qualifier) => {
                if let Some(scope) = scopes.iter().find(|scope| scope.has_qualifier(qualifier)) {
                    scope.require_qualified_all(qualifier, required, control)?;
                }
                return Ok(OwnedSet::new());
            }
            _ => {}
        }
    }
    let mut subquery_ids = OwnedSet::new();
    expression.try_visit(&mut |node| {
        control.check()?;
        match node {
            ScalarExpr::Column(column) => collect_column(None, column, scopes, required, control)?,
            ScalarExpr::QualifiedColumn {
                qualifier, column, ..
            } => collect_column(Some(qualifier), column, scopes, required, control)?,
            ScalarExpr::QualifiedStar(qualifier) => {
                if let Some(scope) = scopes.iter().find(|scope| scope.has_qualifier(qualifier)) {
                    scope.require_qualified_all(qualifier, required, control)?;
                }
            }
            ScalarExpr::ScalarSubquery(subquery)
            | ScalarExpr::Exists { subquery, .. }
            | ScalarExpr::InSubquery { subquery, .. } => {
                insert_set(&mut subquery_ids, *subquery, control)?;
            }
            _ => {}
        }
        Ok::<bool, SQLError>(true)
    })?;
    Ok(subquery_ids)
}

fn collect_expression_and_subqueries(
    expression: &ScalarExpr,
    scopes: &[&SourceLineage],
    direct_star: bool,
    subqueries: &[QueryPlan],
    ctes: &CteScope<'_, impl AsRef<[QueryPlan]>>,
    universe: &mut SourceLineage,
    required: &mut Required,
) -> Result<(), SQLError> {
    for subquery in
        collect_expression_columns(expression, scopes, direct_star, required, &ctes.control())?
    {
        let plan = subqueries.get(subquery).ok_or_else(|| {
            SQLError::Internal(format!(
                "column privilege analysis cannot resolve scalar subquery slot {subquery}"
            ))
        })?;
        analyze_query_plan(plan, ctes, scopes, universe, required)?;
    }
    Ok(())
}

fn analyze_join_source_lineage(
    source: &SourcePlan,
    outer_scopes: &[&SourceLineage],
    subqueries: &[QueryPlan],
    ctes: &CteScope<'_, impl AsRef<[QueryPlan]>>,
    universe: &mut SourceLineage,
    required: &mut Required,
) -> Result<SourceLineage, SQLError> {
    let SourcePlan::Join {
        left,
        right,
        on,
        using,
        natural,
        alias,
        column_aliases,
        ..
    } = source
    else {
        unreachable!("join lineage helper requires a join source")
    };
    let left = analyze_source_lineage(left, outer_scopes, subqueries, ctes, universe, required)?;
    let right_scopes = scopes(&left, outer_scopes, ctes.control())?;
    let right = analyze_source_lineage(right, &right_scopes, subqueries, ctes, universe, required)?;
    if let Some(on) = on {
        let inputs = join_lineage(
            left.copy(&ctes.control())?,
            right.copy(&ctes.control())?,
            None,
            false,
            None,
            &[],
            &ctes.control(),
        )?;
        let condition_scopes = scopes(&inputs, outer_scopes, ctes.control())?;
        collect_expression_and_subqueries(
            on,
            &condition_scopes,
            false,
            subqueries,
            ctes,
            universe,
            required,
        )?;
    }
    if let Some(using) = using {
        for column in &using.columns {
            left.require_unqualified(column, required, &ctes.control())?;
            right.require_unqualified(column, required, &ctes.control())?;
        }
    } else if *natural {
        for column in left
            .output
            .iter()
            .filter(|left| right.output.iter().any(|right| right.name == left.name))
        {
            left.require_unqualified(&column.name, required, &ctes.control())?;
            right.require_unqualified(&column.name, required, &ctes.control())?;
        }
    }
    join_lineage(
        left,
        right,
        using.as_ref(),
        *natural,
        alias.as_deref(),
        column_aliases,
        &ctes.control(),
    )
}

fn analyze_function_group_lineage(
    source: &SourcePlan,
    outer_scopes: &[&SourceLineage],
    subqueries: &[QueryPlan],
    ctes: &CteScope<'_, impl AsRef<[QueryPlan]>>,
    universe: &mut SourceLineage,
    required: &mut Required,
) -> Result<SourceLineage, SQLError> {
    let SourcePlan::FunctionGroup {
        functions,
        alias,
        column_aliases,
        ..
    } = source
    else {
        unreachable!("function-group lineage helper requires a function group")
    };
    for expression in functions.iter().flat_map(|function| &function.args) {
        collect_expression_and_subqueries(
            expression,
            outer_scopes,
            false,
            subqueries,
            ctes,
            universe,
            required,
        )?;
    }
    let control = ctes.control();
    let columns = if column_aliases.is_empty() {
        opaque_columns(
            functions.iter().flat_map(|function| {
                let columns = if function.column_aliases.is_empty() {
                    std::slice::from_ref(&function.output_name)
                } else {
                    function.column_aliases.as_slice()
                };
                columns.iter().map(String::as_str)
            }),
            &control,
        )?
    } else {
        opaque_columns(column_aliases.iter().map(String::as_str), &control)?
    };
    opaque_lineage(columns, alias.as_deref(), &control)
}

fn analyze_source_lineage(
    source: &SourcePlan,
    outer_scopes: &[&SourceLineage],
    subqueries: &[QueryPlan],
    ctes: &CteScope<'_, impl AsRef<[QueryPlan]>>,
    universe: &mut SourceLineage,
    required: &mut Required,
) -> Result<SourceLineage, SQLError> {
    match source {
        crate::plan::SourcePlan::RetainedRows(_) => Ok(SourceLineage::default()),
        SourcePlan::Table {
            name,
            qualifier,
            alias,
            column_aliases,
            ..
        } => table_lineage(name, qualifier, alias.as_deref(), column_aliases, ctes),
        source @ SourcePlan::Join { .. } => {
            analyze_join_source_lineage(source, outer_scopes, subqueries, ctes, universe, required)
        }
        SourcePlan::Values {
            rows,
            alias,
            column_aliases,
            ..
        } => {
            for expression in rows.iter().flatten() {
                collect_expression_and_subqueries(
                    expression,
                    outer_scopes,
                    false,
                    subqueries,
                    ctes,
                    universe,
                    required,
                )?;
            }
            values_lineage(rows, alias.as_deref(), column_aliases, &ctes.control())
        }
        SourcePlan::Function {
            name,
            output_name,
            args,
            alias,
            column_aliases,
            ..
        } => {
            for expression in args {
                collect_expression_and_subqueries(
                    expression,
                    outer_scopes,
                    false,
                    subqueries,
                    ctes,
                    universe,
                    required,
                )?;
            }
            function_lineage(
                name,
                output_name,
                alias.as_deref(),
                column_aliases,
                &ctes.control(),
            )
        }
        source @ SourcePlan::FunctionGroup { .. } => analyze_function_group_lineage(
            source,
            outer_scopes,
            subqueries,
            ctes,
            universe,
            required,
        ),
        SourcePlan::Subquery {
            body,
            alias,
            column_aliases,
            ..
        } => {
            analyze_query_plan(body, ctes, outer_scopes, universe, required)?;
            let columns = query_columns(body, column_aliases, &ctes.control())?;
            opaque_lineage(columns, alias.as_deref(), &ctes.control())
        }
    }
}

fn analyze_query_block(
    block: &QueryBlockPlan,
    source: Option<&SourcePlan>,
    ctes: &CteScope<'_, impl AsRef<[QueryPlan]>>,
    outer_scopes: &[&SourceLineage],
    universe: &mut SourceLineage,
    required: &mut Required,
) -> Result<(), SQLError> {
    let lineage = source.map_or_else(
        || Ok(SourceLineage::default()),
        |source| {
            analyze_source_lineage(
                source,
                outer_scopes,
                &block.subqueries,
                ctes,
                universe,
                required,
            )
        },
    )?;
    universe.include_tables(&lineage, &ctes.control())?;
    let scopes = scopes(&lineage, outer_scopes, ctes.control())?;
    for column in block
        .privilege_columns
        .iter()
        .filter(|_| ctes.include_authorization_only_columns)
    {
        collect_column(
            column.qualifier(),
            column.column(),
            &scopes,
            required,
            &ctes.control(),
        )?;
    }
    for projection in &block.projections {
        collect_expression_and_subqueries(
            &projection.expr,
            &scopes,
            true,
            &block.subqueries,
            ctes,
            universe,
            required,
        )?;
    }
    for expression in block
        .r#where
        .iter()
        .chain(block.group_by.iter())
        .chain(block.grouping_sets.iter().flatten())
        .chain(block.having.iter())
        .chain(block.order_by.iter().map(|order| &order.expr))
        .chain(block.distinct_on.iter())
        .chain(block.limit.iter())
        .chain(block.offset.iter())
    {
        collect_expression_and_subqueries(
            expression,
            &scopes,
            false,
            &block.subqueries,
            ctes,
            universe,
            required,
        )?;
    }
    Ok(())
}

fn analyze_query_plan(
    plan: &QueryPlan,
    ctes: &CteScope<'_, impl AsRef<[QueryPlan]>>,
    outer_scopes: &[&SourceLineage],
    universe: &mut SourceLineage,
    required: &mut Required,
) -> Result<(), SQLError> {
    let mut root_ctes = ctes.reborrow()?;
    root_ctes
        .resolution
        .set_lookup_mode(if plan.relations_bound {
            crate::catalog::resolution::RelationLookupMode::Bound
        } else {
            crate::catalog::resolution::RelationLookupMode::Dynamic
        });
    if let Some(subject) = &plan.relation_privilege_subject {
        root_ctes.relation_privilege_subject = Some(subject.clone_with_control(&ctes.control())?);
        let mut owned_universe = SourceLineage::default();
        let mut owned_required = Required::new();
        analyze_query_plan_in_scope(
            plan,
            &root_ctes,
            outer_scopes,
            &mut owned_universe,
            &mut owned_required,
        )?;
        return ensure_required_select(&owned_universe, &owned_required, &root_ctes);
    }
    analyze_query_plan_in_scope(plan, &root_ctes, outer_scopes, universe, required)
}

fn analyze_query_plan_in_scope(
    plan: &QueryPlan,
    ctes: &CteScope<'_, impl AsRef<[QueryPlan]>>,
    outer_scopes: &[&SourceLineage],
    universe: &mut SourceLineage,
    required: &mut Required,
) -> Result<(), SQLError> {
    let reachable =
        crate::semantics::cte_order::analysis::CteAnalysis::query(plan, ctes.control())?
            .reachable()?;
    let mut root_ctes = ctes.reborrow()?;
    let mut preceding_ctes = root_ctes.reborrow()?;
    for (index, cte) in plan.ctes.iter().enumerate() {
        if reachable[index] {
            let mut definition_ctes = preceding_ctes.reborrow()?;
            if cte.recursive {
                for local in &plan.ctes {
                    definition_ctes.insert_deferred_ref(local)?;
                }
            }
            if let Some(query) = cte.body.query() {
                analyze_query_plan(query, &definition_ctes, outer_scopes, universe, required)?;
            }
        }
        preceding_ctes.insert_deferred_ref(cte)?;
    }
    for cte in &plan.ctes {
        root_ctes.insert_deferred_ref(cte)?;
    }
    match &plan.root {
        RelationalPlan::QueryBlock(block) => analyze_query_block(
            block,
            block.from.as_ref(),
            &root_ctes,
            outer_scopes,
            universe,
            required,
        ),
        RelationalPlan::SetOp {
            left,
            right,
            order_by,
            limit,
            offset,
            subqueries,
            ..
        } => {
            analyze_query_plan(left, &root_ctes, outer_scopes, universe, required)?;
            analyze_query_plan(right, &root_ctes, outer_scopes, universe, required)?;
            let result_scope = opaque_lineage(
                query_columns(left, &[], &ctes.control())?,
                None,
                &ctes.control(),
            )?;
            let scopes = scopes(&result_scope, outer_scopes, ctes.control())?;
            for expression in order_by
                .iter()
                .map(|order| &order.expr)
                .chain(limit.iter().map(Box::as_ref))
                .chain(offset.iter().map(Box::as_ref))
            {
                collect_expression_and_subqueries(
                    expression, &scopes, false, subqueries, &root_ctes, universe, required,
                )?;
            }
            Ok(())
        }
        RelationalPlan::Values { rows, subqueries } => {
            for expression in rows.iter().flatten() {
                collect_expression_and_subqueries(
                    expression,
                    outer_scopes,
                    false,
                    subqueries,
                    &root_ctes,
                    universe,
                    required,
                )?;
            }
            Ok(())
        }
    }
}

pub fn ensure_select_privileges_for_query(
    query: &QueryPlan,
    ctes: &CteScope<'_, impl AsRef<[QueryPlan]>>,
) -> Result<(), SQLError> {
    let mut universe = SourceLineage::default();
    let mut required = Required::new();
    analyze_query_plan(query, ctes, &[], &mut universe, &mut required)?;
    ensure_required_select(&universe, &required, ctes)
}

pub fn ensure_select_privileges_for_query_block(
    statement: &QueryBlockPlan,
    source: &SourcePlan,
    ctes: &CteScope<'_, impl AsRef<[QueryPlan]>>,
) -> Result<(), SQLError> {
    let mut universe = SourceLineage::default();
    let mut required = Required::new();
    analyze_query_block(
        statement,
        Some(source),
        ctes,
        &[],
        &mut universe,
        &mut required,
    )?;
    ensure_required_select(&universe, &required, ctes)
}

pub fn ensure_select_privileges_for_source_expressions(
    source: &SourcePlan,
    expressions: &[&ScalarExpr],
    ctes: &CteScope<'_, impl AsRef<[QueryPlan]>>,
) -> Result<(), SQLError> {
    let (universe, required) = source_expression_dependencies(source, expressions, ctes)?;
    ensure_required_select(&universe, &required, ctes)
}

fn source_expression_dependencies(
    source: &SourcePlan,
    expressions: &[&ScalarExpr],
    ctes: &CteScope<'_, impl AsRef<[QueryPlan]>>,
) -> Result<(SourceLineage, Required), SQLError> {
    let mut universe = SourceLineage::default();
    let mut required = Required::new();
    let lineage = analyze_source_lineage(
        source,
        &[],
        ctes.scalar_subqueries.as_ref(),
        ctes,
        &mut universe,
        &mut required,
    )?;
    universe.include_tables(&lineage, &ctes.control())?;
    let scopes = [&lineage];
    for expression in expressions {
        collect_expression_and_subqueries(
            expression,
            &scopes,
            matches!(expression, ScalarExpr::QualifiedStar(_)),
            ctes.scalar_subqueries.as_ref(),
            ctes,
            &mut universe,
            &mut required,
        )?;
    }
    Ok((universe, required))
}

pub fn ensure_select_privileges_for_table_expressions(
    table: &str,
    qualifiers: &BTreeSet<String>,
    expressions: &[&ScalarExpr],
    subqueries: &[QueryPlan],
    required_columns: &[String],
    ctes: &CteScope<'_, impl AsRef<[QueryPlan]>>,
) -> Result<(), SQLError> {
    let (universe, required) = table_expression_dependencies(
        table,
        qualifiers,
        expressions,
        subqueries,
        required_columns,
        ctes,
    )?;
    ensure_required_select(&universe, &required, ctes)
}

fn table_expression_dependencies(
    table: &str,
    qualifiers: &BTreeSet<String>,
    expressions: &[&ScalarExpr],
    subqueries: &[QueryPlan],
    required_columns: &[String],
    ctes: &CteScope<'_, impl AsRef<[QueryPlan]>>,
) -> Result<(SourceLineage, Required), SQLError> {
    let control = ctes.control();
    let mut resolution = ctes.resolution.copy_with_control(&control)?;
    resolution.set_lookup_mode(crate::catalog::resolution::RelationLookupMode::Bound);
    let Some(relation) = ctes
        .catalog
        .relation_with_control(&resolution, table, &control)?
    else {
        return Err(SQLError::UnknownTable(table.to_string()));
    };
    let lineage = relation_lineage(
        &relation,
        qualifiers.iter().map(String::as_str),
        &[],
        &control,
    )?;
    let mut universe = SourceLineage::default();
    let mut required = Required::new();
    for column in required_columns {
        require(&mut required, &relation.canonical, column, &control)?;
    }
    let scopes = [&lineage];
    for expression in expressions {
        collect_expression_and_subqueries(
            expression,
            &scopes,
            true,
            subqueries,
            ctes,
            &mut universe,
            &mut required,
        )?;
    }
    if required
        .iter()
        .any(|column| column.table.as_str() == relation.canonical)
    {
        universe.include_tables(&lineage, &ctes.control())?;
    }
    Ok((universe, required))
}

pub struct TargetSelectPrivilegeRequest<'a, 'expr> {
    pub table: &'a str,
    pub privilege_subject: Option<&'a RoleReference>,
    pub target_qualifier: &'a str,
    pub returning_aliases: &'a crate::ast::ReturningAliases,
    pub expressions: &'a [&'expr ScalarExpr],
    pub subqueries: &'a [QueryPlan],
    pub required_columns: &'a [String],
}
