//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical-plan correlation analysis for scalar subquery initialization.

use std::collections::{BTreeMap, BTreeSet};

use uqa_execution::{ScalarExpr, ScalarFrameBound};
use uqa_planner::{ComputePlan, ProjectionPlan, QueryPlan, RelationalPlan, SourcePlan};
use uqa_sql::SQLError;

use super::{projection_columns, Engine};

#[derive(Clone, Default)]
struct RelationColumns {
    names: BTreeSet<String>,
    ordered: Vec<String>,
    complete: bool,
}

impl RelationColumns {
    fn known(ordered: Vec<String>) -> Self {
        Self {
            names: ordered.iter().cloned().collect(),
            ordered,
            complete: true,
        }
    }

    fn empty_known() -> Self {
        Self::known(Vec::new())
    }

    fn apply_positional_aliases(&mut self, aliases: &[String]) {
        for (column, alias) in self.ordered.iter_mut().zip(aliases) {
            column.clone_from(alias);
        }
        self.names = self.ordered.iter().cloned().collect();
    }
}

#[derive(Clone, Default)]
struct QueryScope {
    qualifiers: BTreeSet<String>,
    internal_relations: BTreeSet<uqa_sql::ast::InternalRelationId>,
    columns: RelationColumns,
}

pub(super) struct DecorrelatedExistsPlan {
    pub(super) inner: QueryPlan,
    pub(super) outer_keys: Vec<ScalarExpr>,
}

/// Turn a simple correlated equality EXISTS into an uncorrelated key query.
///
/// The caller materializes the returned inner key rows once and probes them
/// with `outer_keys`, which is the physical equivalent of a hash semi-join.
pub(super) fn decorrelate_exists(
    engine: &Engine,
    plan: &QueryPlan,
) -> Result<Option<DecorrelatedExistsPlan>, SQLError> {
    if !plan.ctes.is_empty() {
        return Ok(None);
    }
    let RelationalPlan::QueryBlock(block) = &plan.root else {
        return Ok(None);
    };
    let Some(source) = block.from.as_ref() else {
        return Ok(None);
    };
    if !matches!(block.compute, ComputePlan::Project)
        || !block.group_by.is_empty()
        || !block.grouping_sets.is_empty()
        || block.having.is_some()
        || block.limit.is_some()
        || block.offset.is_some()
        || block.distinct
        || !block.distinct_on.is_empty()
        || !block.subqueries.is_empty()
    {
        return Ok(None);
    }

    let scope = source_scope(engine, source, &BTreeMap::new())?;
    let mut source_scopes = vec![scope.clone()];
    if source_has_external_reference(engine, source, &mut source_scopes)? {
        return Ok(None);
    }
    let Some(predicate) = block.r#where.as_ref() else {
        return Ok(None);
    };
    let conjuncts = match predicate {
        ScalarExpr::And(items) => items.as_slice(),
        expression => std::slice::from_ref(expression),
    };
    let mut inner_keys = Vec::new();
    let mut outer_keys = Vec::new();
    let mut residual = Vec::new();
    for conjunct in conjuncts {
        if let ScalarExpr::Binary {
            op: uqa_sql::ast::BinaryOp::Equal,
            lhs,
            rhs,
        } = conjunct
        {
            let lhs_scope = correlation_column_scope(lhs, &scope);
            let rhs_scope = correlation_column_scope(rhs, &scope);
            match (lhs_scope, rhs_scope) {
                (Some(ColumnScope::Inner), Some(ColumnScope::Outer)) => {
                    inner_keys.push((**lhs).clone());
                    outer_keys.push((**rhs).clone());
                    continue;
                }
                (Some(ColumnScope::Outer), Some(ColumnScope::Inner)) => {
                    inner_keys.push((**rhs).clone());
                    outer_keys.push((**lhs).clone());
                    continue;
                }
                _ => {}
            }
        }
        if expression_has_external_reference(conjunct, std::slice::from_ref(&scope)) {
            return Ok(None);
        }
        residual.push(conjunct.clone());
    }
    if inner_keys.is_empty() {
        return Ok(None);
    }

    let mut inner = plan.clone();
    let RelationalPlan::QueryBlock(inner_block) = &mut inner.root else {
        unreachable!("query-block shape checked above");
    };
    inner_block.projections = inner_keys
        .into_iter()
        .map(|expr| ProjectionPlan { expr, alias: None })
        .collect();
    inner_block.r#where = match residual.len() {
        0 => None,
        1 => residual.pop(),
        _ => Some(ScalarExpr::And(residual)),
    };
    inner_block.order_by.clear();
    Ok(Some(DecorrelatedExistsPlan { inner, outer_keys }))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ColumnScope {
    Inner,
    Outer,
}

fn correlation_column_scope(expression: &ScalarExpr, scope: &QueryScope) -> Option<ColumnScope> {
    match expression {
        ScalarExpr::Column(column) => {
            if scope
                .columns
                .names
                .iter()
                .any(|local| local.eq_ignore_ascii_case(column))
                || scope
                    .qualifiers
                    .iter()
                    .any(|local| local.eq_ignore_ascii_case(column))
            {
                Some(ColumnScope::Inner)
            } else if scope.columns.complete {
                Some(ColumnScope::Outer)
            } else {
                None
            }
        }
        ScalarExpr::QualifiedColumn { qualifier, .. } => {
            if scope
                .qualifiers
                .iter()
                .any(|local| local.eq_ignore_ascii_case(qualifier))
            {
                Some(ColumnScope::Inner)
            } else {
                Some(ColumnScope::Outer)
            }
        }
        ScalarExpr::InternalColumn(column) => {
            if scope.internal_relations.contains(&column.relation()) {
                Some(ColumnScope::Inner)
            } else {
                Some(ColumnScope::Outer)
            }
        }
        ScalarExpr::Cast { expr, .. } => correlation_column_scope(expr, scope),
        _ => None,
    }
}

pub(super) fn query_depends_on_outer_row(
    engine: &Engine,
    plan: &QueryPlan,
) -> Result<bool, SQLError> {
    query_has_external_reference(engine, plan, &mut Vec::new())
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves scope and subquery identity"
)]
fn query_has_external_reference(
    engine: &Engine,
    plan: &QueryPlan,
    scopes: &mut Vec<QueryScope>,
) -> Result<bool, SQLError> {
    let mut ctes = BTreeMap::new();
    if plan.ctes.iter().any(|cte| cte.recursive) {
        for cte in &plan.ctes {
            let columns = if cte.columns.is_empty() {
                query_output_columns(&cte.query)
            } else {
                RelationColumns::known(cte.columns.clone())
            };
            ctes.insert(cte.name.clone(), columns);
        }
    }
    for cte in &plan.ctes {
        let columns = if cte.columns.is_empty() {
            query_output_columns(&cte.query)
        } else {
            RelationColumns::known(cte.columns.clone())
        };
        if cte.recursive {
            ctes.insert(cte.name.clone(), columns.clone());
        }
        if query_has_external_reference(engine, &cte.query, scopes)? {
            return Ok(true);
        }
        ctes.insert(cte.name.clone(), columns);
    }

    match &plan.root {
        RelationalPlan::QueryBlock(block) => {
            let scope = match block.from.as_ref() {
                Some(source) => source_scope(engine, source, &ctes)?,
                None => QueryScope {
                    qualifiers: BTreeSet::new(),
                    internal_relations: BTreeSet::new(),
                    columns: RelationColumns::empty_known(),
                },
            };
            scopes.push(scope);
            let result = (|| {
                for expression in block.expressions() {
                    if expression_has_external_reference(expression, scopes) {
                        return Ok(true);
                    }
                }
                if let Some(source) = block.from.as_ref() {
                    if source_has_external_reference(engine, source, scopes)? {
                        return Ok(true);
                    }
                }
                for subquery in &block.subqueries {
                    if query_has_external_reference(engine, subquery, scopes)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            })();
            scopes.pop();
            result
        }
        RelationalPlan::SetOp {
            left,
            right,
            order_by,
            limit,
            offset,
            subqueries,
            ..
        } => {
            if query_has_external_reference(engine, left, scopes)?
                || query_has_external_reference(engine, right, scopes)?
            {
                return Ok(true);
            }
            scopes.push(QueryScope {
                qualifiers: BTreeSet::new(),
                internal_relations: BTreeSet::new(),
                columns: query_output_columns(left),
            });
            let result = (|| {
                for expression in order_by.iter().map(|order| &order.expr) {
                    if expression_has_external_reference(expression, scopes) {
                        return Ok(true);
                    }
                }
                if limit
                    .as_deref()
                    .is_some_and(|expr| expression_has_external_reference(expr, scopes))
                    || offset
                        .as_deref()
                        .is_some_and(|expr| expression_has_external_reference(expr, scopes))
                {
                    return Ok(true);
                }
                for subquery in subqueries {
                    if query_has_external_reference(engine, subquery, scopes)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            })();
            scopes.pop();
            result
        }
        RelationalPlan::Values { rows, subqueries } => {
            scopes.push(QueryScope {
                qualifiers: BTreeSet::new(),
                internal_relations: BTreeSet::new(),
                columns: RelationColumns::empty_known(),
            });
            let result = (|| {
                for expression in rows.iter().flatten() {
                    if expression_has_external_reference(expression, scopes) {
                        return Ok(true);
                    }
                }
                for subquery in subqueries {
                    if query_has_external_reference(engine, subquery, scopes)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            })();
            scopes.pop();
            result
        }
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves scope and subquery identity"
)]
fn source_scope(
    engine: &Engine,
    source: &SourcePlan,
    ctes: &BTreeMap<String, RelationColumns>,
) -> Result<QueryScope, SQLError> {
    match source {
        SourcePlan::Table {
            name,
            qualifier,
            alias,
            column_aliases,
            ..
        } => {
            let mut qualifiers = BTreeSet::new();
            qualifiers.insert(alias.as_ref().unwrap_or(qualifier).clone());
            let mut columns = relation_columns(engine, name, ctes)?;
            columns.apply_positional_aliases(column_aliases);
            Ok(QueryScope {
                qualifiers,
                internal_relations: BTreeSet::new(),
                columns,
            })
        }
        SourcePlan::Join {
            left,
            right,
            alias,
            column_aliases,
            ..
        } => {
            let left = source_scope(engine, left, ctes)?;
            let right = source_scope(engine, right, ctes)?;
            let complete = left.columns.complete && right.columns.complete;
            let mut names = left.columns.names;
            names.extend(right.columns.names);
            let mut ordered = left.columns.ordered;
            ordered.extend(right.columns.ordered);
            let mut internal_relations = left.internal_relations;
            internal_relations.extend(right.internal_relations);
            if let Some(alias) = alias {
                if !column_aliases.is_empty() {
                    names = column_aliases.iter().cloned().collect();
                    ordered.clone_from(column_aliases);
                }
                return Ok(QueryScope {
                    qualifiers: [alias.clone()].into_iter().collect(),
                    internal_relations,
                    columns: RelationColumns {
                        names,
                        ordered,
                        complete: complete && column_aliases.is_empty(),
                    },
                });
            }
            let mut qualifiers = left.qualifiers;
            qualifiers.extend(right.qualifiers);
            Ok(QueryScope {
                qualifiers,
                internal_relations,
                columns: RelationColumns {
                    names,
                    ordered,
                    complete,
                },
            })
        }
        SourcePlan::Values {
            rows,
            alias,
            column_aliases,
            internal_relation,
            ..
        } => {
            if let Some(internal_relation) = internal_relation {
                return Ok(QueryScope {
                    qualifiers: BTreeSet::new(),
                    internal_relations: [*internal_relation].into_iter().collect(),
                    columns: RelationColumns::empty_known(),
                });
            }
            let qualifiers = alias.iter().cloned().collect();
            let columns = if column_aliases.is_empty() {
                (0..rows.first().map_or(0, Vec::len))
                    .map(|index| format!("column{}", index + 1))
                    .collect::<Vec<_>>()
            } else {
                column_aliases.clone()
            };
            Ok(QueryScope {
                qualifiers,
                internal_relations: BTreeSet::new(),
                columns: RelationColumns::known(columns),
            })
        }
        SourcePlan::Function {
            name,
            output_name,
            alias,
            column_aliases,
            ..
        } => {
            let qualifiers = [alias.as_ref().unwrap_or(output_name).clone()]
                .into_iter()
                .collect();
            let mut names: BTreeSet<String> = column_aliases.iter().cloned().collect();
            let complete = !column_aliases.is_empty()
                || matches!(
                    name.to_ascii_lowercase().as_str(),
                    "generate_series" | "unnest" | "regexp_split_to_table" | "string_to_table"
                );
            if names.is_empty() && complete {
                names.insert(output_name.clone());
            }
            Ok(QueryScope {
                qualifiers,
                internal_relations: BTreeSet::new(),
                columns: RelationColumns {
                    ordered: names.iter().cloned().collect(),
                    names,
                    complete,
                },
            })
        }
        SourcePlan::FunctionGroup {
            functions,
            alias,
            column_aliases,
            ordinality,
        } => {
            let qualifier = alias.clone().or_else(|| {
                functions
                    .first()
                    .map(|function| function.output_name.clone())
            });
            let qualifiers = qualifier.into_iter().collect();
            let mut names = Vec::new();
            let mut complete = true;
            for function in functions {
                if function.column_aliases.is_empty() {
                    let local = crate::sql::builtin_function_dispatch_name(&function.name);
                    if matches!(
                        local.as_str(),
                        "generate_series" | "unnest" | "regexp_split_to_table" | "string_to_table"
                    ) {
                        names.push(function.output_name.clone());
                    } else {
                        complete = false;
                    }
                } else {
                    names.extend(function.column_aliases.iter().cloned());
                }
            }
            if *ordinality {
                names.push("ordinality".into());
            }
            for (name, alias) in names.iter_mut().zip(column_aliases) {
                name.clone_from(alias);
            }
            let column_names = names.iter().cloned().collect();
            Ok(QueryScope {
                qualifiers,
                internal_relations: BTreeSet::new(),
                columns: RelationColumns {
                    names: column_names,
                    ordered: names,
                    complete,
                },
            })
        }
        SourcePlan::Subquery {
            body,
            alias,
            column_aliases,
        } => {
            let qualifiers = alias.iter().cloned().collect();
            let columns = if column_aliases.is_empty() {
                query_output_columns(body)
            } else {
                RelationColumns {
                    names: column_aliases.iter().cloned().collect(),
                    ordered: column_aliases.clone(),
                    complete: true,
                }
            };
            Ok(QueryScope {
                qualifiers,
                internal_relations: BTreeSet::new(),
                columns,
            })
        }
    }
}

fn relation_columns(
    engine: &Engine,
    name: &str,
    ctes: &BTreeMap<String, RelationColumns>,
) -> Result<RelationColumns, SQLError> {
    if let Some(columns) = ctes
        .iter()
        .find(|(cte, _)| cte.eq_ignore_ascii_case(name))
        .map(|(_, columns)| columns)
    {
        return Ok(columns.clone());
    }
    let catalog = engine.catalog_read_view();
    let resolution = engine.session_execution_view().relation_name_resolution();
    if let Some(columns) = super::virtual_relation_schema(&catalog, &resolution, name)? {
        return Ok(RelationColumns::known(
            columns.into_iter().map(|(name, _)| name).collect(),
        ));
    }
    if let Ok(columns) = engine.try_table_columns(name) {
        return Ok(RelationColumns::known(columns));
    }
    if let Some(view) = engine.view_plan(name)? {
        return Ok(query_output_columns(&view));
    }
    if let Some(view) = engine.view_definition(name)? {
        return Ok(RelationColumns::known(
            view.output_columns.unwrap_or_default(),
        ));
    }
    if let Ok(columns) = engine.foreign_table_columns(name) {
        return Ok(RelationColumns::known(columns));
    }
    Ok(RelationColumns::default())
}

fn source_has_external_reference(
    engine: &Engine,
    source: &SourcePlan,
    scopes: &mut Vec<QueryScope>,
) -> Result<bool, SQLError> {
    match source {
        SourcePlan::Join {
            left, right, on, ..
        } => Ok(source_has_external_reference(engine, left, scopes)?
            || source_has_external_reference(engine, right, scopes)?
            || on
                .as_ref()
                .is_some_and(|expr| expression_has_external_reference(expr, scopes))),
        SourcePlan::Values { rows, .. } => Ok(rows
            .iter()
            .flatten()
            .any(|expr| expression_has_external_reference(expr, scopes))),
        SourcePlan::Function { args, .. } => Ok(args
            .iter()
            .any(|expr| expression_has_external_reference(expr, scopes))),
        SourcePlan::FunctionGroup { functions, .. } => Ok(functions.iter().any(|function| {
            function
                .args
                .iter()
                .any(|expr| expression_has_external_reference(expr, scopes))
        })),
        SourcePlan::Subquery { body, .. } => query_has_external_reference(engine, body, scopes),
        SourcePlan::Table { .. } => Ok(false),
    }
}

fn query_output_columns(plan: &QueryPlan) -> RelationColumns {
    match &plan.root {
        RelationalPlan::QueryBlock(block) => {
            let ordered = projection_columns(&block.projections);
            RelationColumns {
                names: ordered.iter().cloned().collect(),
                ordered,
                complete: !block
                    .projections
                    .iter()
                    .any(|projection| matches!(projection.expr, ScalarExpr::Star)),
            }
        }
        RelationalPlan::SetOp { left, .. } => query_output_columns(left),
        RelationalPlan::Values { rows, .. } => RelationColumns::known(
            (0..rows.first().map_or(0, Vec::len))
                .map(|index| format!("column{}", index + 1))
                .collect(),
        ),
    }
}

fn expression_has_external_reference(expr: &ScalarExpr, scopes: &[QueryScope]) -> bool {
    match expr {
        ScalarExpr::Column(column) => !resolves_unqualified(column, scopes),
        ScalarExpr::QualifiedColumn { qualifier, .. } => !scopes.iter().rev().any(|scope| {
            scope
                .qualifiers
                .iter()
                .any(|local| local.eq_ignore_ascii_case(qualifier))
        }),
        ScalarExpr::QualifiedStar(qualifier) => !scopes.iter().rev().any(|scope| {
            scope
                .qualifiers
                .iter()
                .any(|local| local.eq_ignore_ascii_case(qualifier))
        }),
        ScalarExpr::InternalColumn(column) => !scopes
            .iter()
            .rev()
            .any(|scope| scope.internal_relations.contains(&column.relation())),
        ScalarExpr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            args.iter()
                .any(|expr| expression_has_external_reference(expr, scopes))
                || order_by
                    .iter()
                    .any(|order| expression_has_external_reference(&order.expr, scopes))
                || filter
                    .as_deref()
                    .is_some_and(|expr| expression_has_external_reference(expr, scopes))
        }
        ScalarExpr::Array(items)
        | ScalarExpr::Row(items)
        | ScalarExpr::And(items)
        | ScalarExpr::Or(items) => items
            .iter()
            .any(|expr| expression_has_external_reference(expr, scopes)),
        ScalarExpr::Binary { lhs, rhs, .. } => {
            expression_has_external_reference(lhs, scopes)
                || expression_has_external_reference(rhs, scopes)
        }
        ScalarExpr::Not(inner)
        | ScalarExpr::UnaryMinus(inner)
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::Cast { expr: inner, .. } => expression_has_external_reference(inner, scopes),
        ScalarExpr::Between { expr, low, high } => {
            expression_has_external_reference(expr, scopes)
                || expression_has_external_reference(low, scopes)
                || expression_has_external_reference(high, scopes)
        }
        ScalarExpr::InList { expr, list, .. } => {
            expression_has_external_reference(expr, scopes)
                || list
                    .iter()
                    .any(|item| expression_has_external_reference(item, scopes))
        }
        ScalarExpr::WindowCall { args, spec, .. } => {
            args.iter()
                .any(|expr| expression_has_external_reference(expr, scopes))
                || spec
                    .partition_by
                    .iter()
                    .any(|expr| expression_has_external_reference(expr, scopes))
                || spec
                    .order_by
                    .iter()
                    .any(|order| expression_has_external_reference(&order.expr, scopes))
                || spec.frame.as_ref().is_some_and(|frame| {
                    frame_bound_has_external_reference(&frame.start, scopes)
                        || frame_bound_has_external_reference(&frame.end, scopes)
                })
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            base.as_deref()
                .is_some_and(|expr| expression_has_external_reference(expr, scopes))
                || when.iter().any(|(condition, result)| {
                    expression_has_external_reference(condition, scopes)
                        || expression_has_external_reference(result, scopes)
                })
                || else_branch
                    .as_deref()
                    .is_some_and(|expr| expression_has_external_reference(expr, scopes))
        }
        ScalarExpr::InSubquery { expr, .. } => expression_has_external_reference(expr, scopes),
        ScalarExpr::Default
        | ScalarExpr::Star
        | ScalarExpr::Position(_)
        | ScalarExpr::Literal(_)
        | ScalarExpr::Param(_)
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. } => false,
    }
}

fn resolves_unqualified(column: &str, scopes: &[QueryScope]) -> bool {
    for scope in scopes.iter().rev() {
        if scope
            .columns
            .names
            .iter()
            .any(|local| local.eq_ignore_ascii_case(column))
            || scope
                .qualifiers
                .iter()
                .any(|local| local.eq_ignore_ascii_case(column))
        {
            return true;
        }
        if !scope.columns.complete {
            return false;
        }
    }
    false
}

fn frame_bound_has_external_reference(bound: &ScalarFrameBound, scopes: &[QueryScope]) -> bool {
    match bound {
        ScalarFrameBound::Preceding(expr) | ScalarFrameBound::Following(expr) => {
            expression_has_external_reference(expr, scopes)
        }
        ScalarFrameBound::UnboundedPreceding
        | ScalarFrameBound::UnboundedFollowing
        | ScalarFrameBound::CurrentRow => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_range_aliases_replace_physical_correlation_names() {
        let engine = Engine::new();
        engine
            .sql(
                "CREATE TABLE correlation_alias_source(id INTEGER, label TEXT)",
                &[],
            )
            .unwrap();
        let source = SourcePlan::Table {
            name: "correlation_alias_source".into(),
            qualifier: "correlation_alias_source".into(),
            alias: Some("source".into()),
            column_aliases: vec!["key".into(), "value".into()],
            include_descendants: true,
        };
        let scope = source_scope(&engine, &source, &BTreeMap::new()).unwrap();
        assert_eq!(
            scope.columns.names,
            BTreeSet::from(["key".to_string(), "value".to_string()])
        );
    }
}
