//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Semantic base-column privilege analysis for query sources.

mod enforcement;

use std::collections::{BTreeMap, BTreeSet};

use uqa_execution::ScalarExpr;
use uqa_planner::{QueryBlockPlan, QueryPlan, RelationalPlan, SourcePlan};
use uqa_sql::SQLError;

use super::CteScope;
use enforcement::ensure_required_select;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct BaseColumn {
    table: String,
    column: String,
}

#[derive(Clone, Debug)]
struct OutputColumn {
    name: String,
    sources: BTreeSet<BaseColumn>,
}

#[derive(Clone, Default)]
struct SourceLineage {
    output: Vec<OutputColumn>,
    qualifiers: BTreeMap<String, Vec<OutputColumn>>,
    system_qualifiers: BTreeMap<String, BTreeSet<String>>,
    tables: BTreeMap<String, BTreeSet<String>>,
}

impl SourceLineage {
    fn has_unqualified(&self, column: &str) -> bool {
        self.output.iter().any(|output| output.name == column)
    }

    fn has_qualifier(&self, qualifier: &str) -> bool {
        self.qualifiers.contains_key(qualifier) || self.system_qualifiers.contains_key(qualifier)
    }

    fn require_unqualified(&self, column: &str, required: &mut BTreeSet<BaseColumn>) {
        for output in self.output.iter().filter(|output| output.name == column) {
            required.extend(output.sources.iter().cloned());
        }
    }

    fn require_qualified(
        &self,
        qualifier: &str,
        column: &str,
        required: &mut BTreeSet<BaseColumn>,
    ) {
        if let Some(columns) = self.qualifiers.get(qualifier) {
            for output in columns.iter().filter(|output| output.name == column) {
                required.extend(output.sources.iter().cloned());
            }
        }
    }

    fn require_all(&self, required: &mut BTreeSet<BaseColumn>) {
        for output in &self.output {
            required.extend(output.sources.iter().cloned());
        }
    }

    fn require_qualified_all(&self, qualifier: &str, required: &mut BTreeSet<BaseColumn>) {
        if let Some(columns) = self.qualifiers.get(qualifier) {
            for output in columns {
                required.extend(output.sources.iter().cloned());
            }
        }
    }

    fn require_unqualified_system(&self, column: &str, required: &mut BTreeSet<BaseColumn>) {
        for table in self.system_qualifiers.values().flatten() {
            required.insert(BaseColumn {
                table: table.clone(),
                column: column.to_string(),
            });
        }
    }

    fn require_qualified_system(
        &self,
        qualifier: &str,
        column: &str,
        required: &mut BTreeSet<BaseColumn>,
    ) {
        if let Some(tables) = self.system_qualifiers.get(qualifier) {
            required.extend(tables.iter().cloned().map(|table| BaseColumn {
                table,
                column: column.to_string(),
            }));
        }
    }

    fn include_tables(&mut self, other: &Self) {
        for (table, columns) in &other.tables {
            self.tables
                .entry(table.clone())
                .or_default()
                .extend(columns.iter().cloned());
        }
    }
}

fn opaque_lineage(columns: Vec<String>, qualifier: Option<String>) -> SourceLineage {
    let output = columns
        .into_iter()
        .map(|name| OutputColumn {
            name,
            sources: BTreeSet::new(),
        })
        .collect::<Vec<_>>();
    let qualifiers = qualifier.map_or_else(BTreeMap::new, |qualifier| {
        BTreeMap::from([(qualifier, output.clone())])
    });
    SourceLineage {
        output,
        qualifiers,
        system_qualifiers: BTreeMap::new(),
        tables: BTreeMap::new(),
    }
}

fn is_system_column(column: &str) -> bool {
    matches!(
        column,
        "ctid" | "xmin" | "cmin" | "xmax" | "cmax" | "tableoid"
    )
}

fn rename_output_columns(output: &mut [OutputColumn], aliases: &[String]) {
    for (column, alias) in output.iter_mut().zip(aliases) {
        column.name.clone_from(alias);
    }
}

fn table_lineage(
    name: &str,
    qualifier: &str,
    alias: Option<&str>,
    ctes: &CteScope,
) -> Result<SourceLineage, SQLError> {
    if ctes.is_visible_cte(name) {
        let columns = ctes
            .rows
            .get(name)
            .map(|rows| rows.row_schema().columns().to_vec())
            .or_else(|| {
                ctes.deferred_ctes()
                    .get(name)
                    .and_then(|cte| super::query_plan_output_columns(&cte.query))
            })
            .unwrap_or_default();
        return Ok(opaque_lineage(
            columns,
            Some(alias.unwrap_or(qualifier).to_string()),
        ));
    }
    let catalog = ctes.catalog_read_view()?;
    let resolution = ctes.relation_name_resolution()?;
    let (canonical, columns, system_columns) =
        if let Some(canonical) = catalog.table_name_resolved(&resolution, name)? {
            let table = catalog
                .table_resolved(&resolution, &canonical)?
                .ok_or_else(|| SQLError::UnknownTable(canonical.clone()))?;
            (
                canonical,
                table
                    .columns
                    .iter()
                    .map(|column| column.name.clone())
                    .collect::<Vec<_>>(),
                true,
            )
        } else if let Some(canonical) = catalog.view_name_resolved(&resolution, name)? {
            let view = catalog
                .view_resolved(&resolution, &canonical)?
                .ok_or_else(|| SQLError::UnknownTable(canonical.clone()))?;
            let columns = view.output_columns.clone().ok_or_else(|| {
                SQLError::Internal(format!(
                    "loaded view `{canonical}` has no durable public column metadata"
                ))
            })?;
            (canonical, columns, false)
        } else {
            return Ok(SourceLineage::default());
        };
    let visible_qualifier = alias.unwrap_or(qualifier).to_string();
    let output = columns
        .iter()
        .map(|column| OutputColumn {
            name: column.clone(),
            sources: BTreeSet::from([BaseColumn {
                table: canonical.clone(),
                column: column.clone(),
            }]),
        })
        .collect::<Vec<_>>();
    Ok(SourceLineage {
        output: output.clone(),
        qualifiers: BTreeMap::from([(visible_qualifier, output)]),
        system_qualifiers: if system_columns {
            BTreeMap::from([(
                alias.unwrap_or(qualifier).to_string(),
                BTreeSet::from([canonical.clone()]),
            )])
        } else {
            BTreeMap::new()
        },
        tables: BTreeMap::from([(canonical, columns.into_iter().collect())]),
    })
}

fn join_lineage(
    left: SourceLineage,
    right: SourceLineage,
    using: Option<&uqa_sql::ast::JoinUsing>,
    natural: bool,
    alias: Option<&str>,
    column_aliases: &[String],
) -> SourceLineage {
    let using_columns = using.map_or_else(
        || {
            if natural {
                left.output
                    .iter()
                    .filter(|left| right.output.iter().any(|right| right.name == left.name))
                    .map(|column| column.name.clone())
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            }
        },
        |using| using.columns.clone(),
    );
    let using_set = using_columns.iter().collect::<BTreeSet<_>>();
    let mut output = Vec::new();
    for column in &using_columns {
        let mut sources = BTreeSet::new();
        for candidate in left
            .output
            .iter()
            .chain(&right.output)
            .filter(|candidate| candidate.name == *column)
        {
            sources.extend(candidate.sources.iter().cloned());
        }
        output.push(OutputColumn {
            name: column.clone(),
            sources,
        });
    }
    output.extend(
        left.output
            .iter()
            .chain(&right.output)
            .filter(|column| !using_set.contains(&column.name))
            .cloned(),
    );
    let mut tables = left.tables.clone();
    for (table, columns) in &right.tables {
        tables
            .entry(table.clone())
            .or_default()
            .extend(columns.iter().cloned());
    }
    let (qualifiers, system_qualifiers) = if let Some(alias) = alias {
        rename_output_columns(&mut output, column_aliases);
        (
            BTreeMap::from([(alias.to_string(), output.clone())]),
            BTreeMap::new(),
        )
    } else {
        let mut qualifiers = left.qualifiers;
        qualifiers.extend(right.qualifiers);
        let mut system_qualifiers = left.system_qualifiers;
        system_qualifiers.extend(right.system_qualifiers);
        (qualifiers, system_qualifiers)
    };
    SourceLineage {
        output,
        qualifiers,
        system_qualifiers,
        tables,
    }
}

fn collect_expression_columns(
    expression: &ScalarExpr,
    scopes: &[SourceLineage],
    direct_star: bool,
    required: &mut BTreeSet<BaseColumn>,
) -> BTreeSet<usize> {
    if direct_star {
        match expression {
            ScalarExpr::Star => {
                if let Some(scope) = scopes.iter().find(|scope| !scope.output.is_empty()) {
                    scope.require_all(required);
                }
                return BTreeSet::new();
            }
            ScalarExpr::QualifiedStar(qualifier) => {
                if let Some(scope) = scopes.iter().find(|scope| scope.has_qualifier(qualifier)) {
                    scope.require_qualified_all(qualifier, required);
                }
                return BTreeSet::new();
            }
            _ => {}
        }
    }
    let mut subquery_ids = BTreeSet::new();
    expression.visit(&mut |node| match node {
        ScalarExpr::Column(column) => {
            if let Some(scope) = scopes.iter().find(|scope| scope.has_unqualified(column)) {
                scope.require_unqualified(column, required);
            } else if is_system_column(column) {
                if let Some(scope) = scopes
                    .iter()
                    .find(|scope| !scope.system_qualifiers.is_empty())
                {
                    scope.require_unqualified_system(column, required);
                }
            }
        }
        ScalarExpr::QualifiedColumn {
            qualifier, column, ..
        } => {
            if let Some(scope) = scopes.iter().find(|scope| scope.has_qualifier(qualifier)) {
                scope.require_qualified(qualifier, column, required);
                if is_system_column(column) {
                    scope.require_qualified_system(qualifier, column, required);
                }
            }
        }
        ScalarExpr::QualifiedStar(qualifier) => {
            if let Some(scope) = scopes.iter().find(|scope| scope.has_qualifier(qualifier)) {
                scope.require_qualified_all(qualifier, required);
            }
        }
        ScalarExpr::ScalarSubquery(subquery)
        | ScalarExpr::Exists { subquery, .. }
        | ScalarExpr::InSubquery { subquery, .. } => {
            subquery_ids.insert(*subquery);
        }
        _ => {}
    });
    subquery_ids
}

fn collect_expression_and_subqueries(
    expression: &ScalarExpr,
    scopes: &[SourceLineage],
    direct_star: bool,
    subqueries: &[QueryPlan],
    ctes: &CteScope,
    universe: &mut SourceLineage,
    required: &mut BTreeSet<BaseColumn>,
) -> Result<(), SQLError> {
    for subquery in collect_expression_columns(expression, scopes, direct_star, required) {
        let plan = subqueries.get(subquery).ok_or_else(|| {
            SQLError::Internal(format!(
                "column privilege analysis cannot resolve scalar subquery slot {subquery}"
            ))
        })?;
        analyze_query_plan(plan, ctes, scopes, universe, required)?;
    }
    Ok(())
}

fn values_lineage(
    rows: &[Vec<ScalarExpr>],
    alias: Option<&str>,
    column_aliases: &[String],
) -> SourceLineage {
    let columns = if column_aliases.is_empty() {
        (1..=rows.first().map_or(0, Vec::len))
            .map(|index| format!("column{index}"))
            .collect()
    } else {
        column_aliases.to_vec()
    };
    opaque_lineage(columns, alias.map(str::to_string))
}

fn function_lineage(
    name: &str,
    output_name: &str,
    alias: Option<&str>,
    column_aliases: &[String],
) -> SourceLineage {
    let columns = if column_aliases.is_empty() {
        vec![if output_name.is_empty() {
            name.rsplit('.').next().unwrap_or(name).to_string()
        } else {
            output_name.to_string()
        }]
    } else {
        column_aliases.to_vec()
    };
    let qualifier = alias.map_or_else(
        || name.rsplit('.').next().unwrap_or(name).to_string(),
        str::to_string,
    );
    opaque_lineage(columns, Some(qualifier))
}

fn analyze_join_source_lineage(
    source: &SourcePlan,
    outer_scopes: &[SourceLineage],
    subqueries: &[QueryPlan],
    ctes: &CteScope,
    universe: &mut SourceLineage,
    required: &mut BTreeSet<BaseColumn>,
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
    let mut right_scopes = vec![left.clone()];
    right_scopes.extend_from_slice(outer_scopes);
    let right = analyze_source_lineage(right, &right_scopes, subqueries, ctes, universe, required)?;
    let inputs = join_lineage(left.clone(), right.clone(), None, false, None, &[]);
    let mut condition_scopes = vec![inputs];
    condition_scopes.extend_from_slice(outer_scopes);
    if let Some(on) = on {
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
            left.require_unqualified(column, required);
            right.require_unqualified(column, required);
        }
    } else if *natural {
        for column in left
            .output
            .iter()
            .filter(|left| right.output.iter().any(|right| right.name == left.name))
        {
            left.require_unqualified(&column.name, required);
            right.require_unqualified(&column.name, required);
        }
    }
    Ok(join_lineage(
        left,
        right,
        using.as_ref(),
        *natural,
        alias.as_deref(),
        column_aliases,
    ))
}

fn analyze_function_group_lineage(
    source: &SourcePlan,
    outer_scopes: &[SourceLineage],
    subqueries: &[QueryPlan],
    ctes: &CteScope,
    universe: &mut SourceLineage,
    required: &mut BTreeSet<BaseColumn>,
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
    let columns = if column_aliases.is_empty() {
        functions
            .iter()
            .flat_map(|function| {
                if function.column_aliases.is_empty() {
                    vec![function.output_name.clone()]
                } else {
                    function.column_aliases.clone()
                }
            })
            .collect()
    } else {
        column_aliases.clone()
    };
    Ok(opaque_lineage(columns, alias.clone()))
}

fn analyze_source_lineage(
    source: &SourcePlan,
    outer_scopes: &[SourceLineage],
    subqueries: &[QueryPlan],
    ctes: &CteScope,
    universe: &mut SourceLineage,
    required: &mut BTreeSet<BaseColumn>,
) -> Result<SourceLineage, SQLError> {
    match source {
        SourcePlan::Table {
            name,
            qualifier,
            alias,
            ..
        } => table_lineage(name, qualifier, alias.as_deref(), ctes),
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
            Ok(values_lineage(rows, alias.as_deref(), column_aliases))
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
            Ok(function_lineage(
                name,
                output_name,
                alias.as_deref(),
                column_aliases,
            ))
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
        } => {
            analyze_query_plan(body, ctes, outer_scopes, universe, required)?;
            let mut columns = super::query_plan_output_columns(body).unwrap_or_default();
            for (column, alias) in columns.iter_mut().zip(column_aliases) {
                column.clone_from(alias);
            }
            Ok(opaque_lineage(columns, alias.clone()))
        }
    }
}

fn analyze_query_block(
    block: &QueryBlockPlan,
    source: Option<&SourcePlan>,
    ctes: &CteScope,
    outer_scopes: &[SourceLineage],
    universe: &mut SourceLineage,
    required: &mut BTreeSet<BaseColumn>,
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
    universe.include_tables(&lineage);
    let mut scopes = vec![lineage];
    scopes.extend_from_slice(outer_scopes);
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
    ctes: &CteScope,
    outer_scopes: &[SourceLineage],
    universe: &mut SourceLineage,
    required: &mut BTreeSet<BaseColumn>,
) -> Result<(), SQLError> {
    let reachable = super::reachable_plan_cte_names(plan);
    let mut preceding_ctes = ctes.clone();
    for cte in &plan.ctes {
        if reachable.contains(&cte.name) {
            let mut definition_ctes = preceding_ctes.clone();
            if cte.recursive {
                for local in &plan.ctes {
                    definition_ctes.insert_deferred(local.clone());
                }
            }
            analyze_query_plan(
                &cte.query,
                &definition_ctes,
                outer_scopes,
                universe,
                required,
            )?;
        }
        preceding_ctes.insert_deferred(cte.clone());
    }
    let mut root_ctes = ctes.clone();
    for cte in &plan.ctes {
        root_ctes.insert_deferred(cte.clone());
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
                super::query_plan_output_columns(left).unwrap_or_default(),
                None,
            );
            let mut scopes = vec![result_scope];
            scopes.extend_from_slice(outer_scopes);
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

pub(in crate::sql) fn ensure_select_privileges_for_query_block(
    statement: &QueryBlockPlan,
    source: &SourcePlan,
    ctes: &CteScope,
) -> Result<(), SQLError> {
    let mut universe = SourceLineage::default();
    let mut required = BTreeSet::new();
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

pub(in crate::sql) fn ensure_select_privileges_for_source_expressions(
    source: &SourcePlan,
    expressions: &[&ScalarExpr],
    ctes: &CteScope,
) -> Result<(), SQLError> {
    let mut universe = SourceLineage::default();
    let mut required = BTreeSet::new();
    let lineage = analyze_source_lineage(
        source,
        &[],
        &ctes.scalar_subqueries,
        ctes,
        &mut universe,
        &mut required,
    )?;
    universe.include_tables(&lineage);
    let scopes = [lineage.clone()];
    for expression in expressions {
        collect_expression_and_subqueries(
            expression,
            &scopes,
            matches!(expression, ScalarExpr::QualifiedStar(_)),
            &ctes.scalar_subqueries,
            ctes,
            &mut universe,
            &mut required,
        )?;
    }
    ensure_required_select(&universe, &required, ctes)
}

pub(in crate::sql) fn ensure_select_privileges_for_table_expressions(
    table: &str,
    qualifiers: &BTreeSet<String>,
    expressions: &[&ScalarExpr],
    subqueries: &[QueryPlan],
    required_columns: &[String],
    ctes: &CteScope,
) -> Result<(), SQLError> {
    let catalog = ctes.catalog_read_view()?;
    let mut resolution = ctes.relation_name_resolution()?;
    resolution.set_lookup_mode(crate::engine_capabilities::RelationLookupMode::Bound);
    let (canonical, columns, system_columns) =
        if let Some(canonical) = catalog.table_name_resolved(&resolution, table)? {
            let snapshot = catalog
                .table_resolved(&resolution, &canonical)?
                .ok_or_else(|| SQLError::UnknownTable(canonical.clone()))?;
            (
                canonical,
                snapshot
                    .columns
                    .iter()
                    .map(|column| column.name.clone())
                    .collect::<Vec<_>>(),
                true,
            )
        } else if let Some(canonical) = catalog.view_name_resolved(&resolution, table)? {
            let view = catalog
                .view_resolved(&resolution, &canonical)?
                .ok_or_else(|| SQLError::UnknownTable(canonical.clone()))?;
            let columns = view.output_columns.clone().ok_or_else(|| {
                SQLError::Internal(format!(
                    "loaded view `{canonical}` has no durable public column metadata"
                ))
            })?;
            (canonical, columns, false)
        } else {
            return Err(SQLError::UnknownTable(table.to_string()));
        };
    let output = columns
        .iter()
        .map(|column| OutputColumn {
            name: column.clone(),
            sources: BTreeSet::from([BaseColumn {
                table: canonical.clone(),
                column: column.clone(),
            }]),
        })
        .collect::<Vec<_>>();
    let lineage = SourceLineage {
        output: output.clone(),
        qualifiers: qualifiers
            .iter()
            .map(|qualifier| (qualifier.clone(), output.clone()))
            .collect(),
        system_qualifiers: if system_columns {
            qualifiers
                .iter()
                .map(|qualifier| (qualifier.clone(), BTreeSet::from([canonical.clone()])))
                .collect()
        } else {
            BTreeMap::new()
        },
        tables: BTreeMap::from([(canonical.clone(), columns.into_iter().collect())]),
    };
    let mut universe = SourceLineage::default();
    let mut required = required_columns
        .iter()
        .cloned()
        .map(|column| BaseColumn {
            table: canonical.clone(),
            column,
        })
        .collect::<BTreeSet<_>>();
    let scopes = [lineage.clone()];
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
    if required.iter().any(|column| column.table == canonical) {
        universe.include_tables(&lineage);
    }
    ensure_required_select(&universe, &required, ctes)
}
