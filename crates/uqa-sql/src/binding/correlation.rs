//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical-plan correlation analysis for scalar subquery initialization.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::plan::{ComputePlan, ProjectionPlan, QueryPlan, RelationalPlan, SourcePlan};
use crate::SQLError;
use crate::{ScalarExpr, ScalarFrameBound};

use crate::catalog::{analysis::AnalysisCatalog, resolution::RelationNameResolution};
mod outputs;
use outputs::{cte_output_columns, query_output_columns, source_scope};

mod sources;
pub use sources::source_depends_on_current_row;
use sources::{expression_references_outer, source_references_outer};

/// A single immutable namespace and catalog view used throughout correlation analysis.
#[derive(Clone, Copy)]
pub struct CorrelationContext<'a> {
    pub routines: &'a dyn crate::routines::RoutineResolution,
    pub catalog: &'a dyn AnalysisCatalog,
    pub resolution: &'a RelationNameResolution,
    pub scoped_relations: Option<&'a dyn CorrelationRelations>,
}

/// Relation descriptors already retained by the invoking query, including transition relations.
pub trait CorrelationRelations {
    fn columns(&self, relation: &str) -> Option<&[String]>;
}

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
    qualifiers: BTreeMap<String, Arc<RelationColumns>>,
    internal_relations: BTreeSet<crate::ast::InternalRelationId>,
    columns: Arc<RelationColumns>,
    enclosing: Option<crate::RowSchema>,
}

pub struct DecorrelatedExistsPlan {
    pub inner: QueryPlan,
    pub outer_keys: Vec<ScalarExpr>,
}

/// Turn a simple correlated equality EXISTS into an uncorrelated key query.
///
/// The caller materializes the returned inner key rows once and probes them
/// with `outer_keys`, which is the physical equivalent of a hash semi-join.
pub fn decorrelate_exists(
    context: CorrelationContext<'_>,
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
    if !block.deferred_outer_rewrites.is_empty()
        || !matches!(block.compute, ComputePlan::Project)
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

    let scope = source_scope(context, source, &BTreeMap::new(), &[])?;
    if source_references_outer(
        context,
        source,
        &block.subqueries,
        &mut Vec::new(),
        &BTreeMap::new(),
    )? {
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
            op: crate::ast::BinaryOp::Equal,
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
            if scope.columns.names.contains(column.as_str())
                || scope.qualifiers.contains_key(column.as_str())
            {
                Some(ColumnScope::Inner)
            } else if scope.columns.complete {
                Some(ColumnScope::Outer)
            } else {
                None
            }
        }
        ScalarExpr::QualifiedColumn { qualifier, .. } => {
            if scope.qualifiers.contains_key(qualifier) {
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
        ScalarExpr::Cast { expr, .. } | ScalarExpr::Collated { expr, .. } => {
            correlation_column_scope(expr, scope)
        }
        _ => None,
    }
}

pub fn query_depends_on_outer_row(
    context: CorrelationContext<'_>,
    plan: &QueryPlan,
) -> Result<bool, SQLError> {
    query_has_external_reference(context, plan, &mut Vec::new())
}

/// Whether a subquery refers to the caller's own row. Seed enclosing identities so references beyond that row are not mistaken for level-zero variables.
pub(crate) fn query_depends_on_current_row(
    context: CorrelationContext<'_>,
    plan: &QueryPlan,
    input: &crate::RowSchema,
) -> Result<bool, SQLError> {
    query_has_external_reference(context, plan, &mut vec![enclosing_scope(input)])
}

fn enclosing_scope(input: &crate::RowSchema) -> QueryScope {
    let deferred_columns = Arc::new(RelationColumns::default());
    let mut enclosing = QueryScope {
        columns: Arc::new(RelationColumns::empty_known()),
        enclosing: Some(input.clone()),
        ..QueryScope::default()
    };
    for (identity, _) in input.typed_physical_alias_identities() {
        if !input.resolves_local_column(identity.qualifier(), identity.column()) {
            if let Some(qualifier) = identity.qualifier() {
                enclosing
                    .qualifiers
                    .entry(qualifier.to_owned())
                    .or_insert_with(|| Arc::clone(&deferred_columns));
            } else {
                Arc::make_mut(&mut enclosing.columns)
                    .names
                    .insert(identity.column().to_owned());
            }
        }
    }
    for (column, slot) in input.internal_identities() {
        if !input.slot_is_local(slot) {
            enclosing.internal_relations.insert(column.relation());
        }
    }
    enclosing
}

fn query_has_external_reference(
    context: CorrelationContext<'_>,
    plan: &QueryPlan,
    scopes: &mut Vec<QueryScope>,
) -> Result<bool, SQLError> {
    query_references_outer(context, plan, scopes, &BTreeMap::new())
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves scope and subquery identity"
)]
fn query_references_outer(
    context: CorrelationContext<'_>,
    plan: &QueryPlan,
    scopes: &mut Vec<QueryScope>,
    parent_ctes: &BTreeMap<String, RelationColumns>,
) -> Result<bool, SQLError> {
    let mut ctes = parent_ctes.clone();
    if plan.ctes.iter().any(|cte| cte.recursive) {
        for cte in &plan.ctes {
            let columns = RelationColumns {
                names: cte.columns.iter().cloned().collect(),
                ordered: cte.columns.clone(),
                complete: false,
            };
            ctes.insert(cte.name.clone(), columns);
        }
    }
    for cte in &plan.ctes {
        let columns = cte_output_columns(context, cte, &ctes, scopes)?;
        if cte.recursive {
            ctes.insert(cte.name.clone(), columns.clone());
        }
        if cte.body.query().map_or(Ok(true), |query| {
            query_references_outer(context, query, scopes, &ctes)
        })? {
            return Ok(true);
        }
        ctes.insert(cte.name.clone(), columns);
    }

    match &plan.root {
        RelationalPlan::QueryBlock(block) => {
            // A FROM item's own output is not visible to its arguments. Bind its dependencies before adding the complete query namespace used by projections and predicates.
            if let Some(source) = block.from.as_ref() {
                if source_references_outer(context, source, &block.subqueries, scopes, &ctes)? {
                    return Ok(true);
                }
            }
            let scope = match block.from.as_ref() {
                Some(source) => source_scope(context, source, &ctes, scopes)?,
                None => QueryScope {
                    qualifiers: BTreeMap::new(),
                    internal_relations: BTreeSet::new(),
                    columns: Arc::new(RelationColumns::empty_known()),
                    enclosing: None,
                },
            };
            scopes.push(scope);
            let result = (|| {
                for expression in block.expressions() {
                    if expression_references_outer(
                        context,
                        expression,
                        &block.subqueries,
                        scopes,
                        &ctes,
                    )? {
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
            if query_references_outer(context, left, scopes, &ctes)?
                || query_references_outer(context, right, scopes, &ctes)?
            {
                return Ok(true);
            }
            scopes.push(QueryScope {
                qualifiers: BTreeMap::new(),
                internal_relations: BTreeSet::new(),
                columns: Arc::new(query_output_columns(context, left, &ctes, scopes)?),
                enclosing: None,
            });
            let result = (|| {
                for expression in order_by.iter().map(|order| &order.expr) {
                    if expression_references_outer(context, expression, subqueries, scopes, &ctes)?
                    {
                        return Ok(true);
                    }
                }
                for expression in limit.iter().chain(offset).map(AsRef::as_ref) {
                    if expression_references_outer(context, expression, subqueries, scopes, &ctes)?
                    {
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
                qualifiers: BTreeMap::new(),
                internal_relations: BTreeSet::new(),
                columns: Arc::new(RelationColumns::empty_known()),
                enclosing: None,
            });
            let result = (|| {
                for expression in rows.iter().flatten() {
                    if expression_references_outer(context, expression, subqueries, scopes, &ctes)?
                    {
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

fn expression_has_external_reference(expr: &ScalarExpr, scopes: &[QueryScope]) -> bool {
    match expr {
        ScalarExpr::Column(column) => !resolves_unqualified(column, scopes),
        ScalarExpr::QualifiedColumn { qualifier, .. } => !scopes
            .iter()
            .rev()
            .any(|scope| scope.qualifiers.contains_key(qualifier)),
        ScalarExpr::QualifiedStar(qualifier) => !scopes
            .iter()
            .rev()
            .any(|scope| scope.qualifiers.contains_key(qualifier.as_str())),
        ScalarExpr::InternalColumn(column) => !scopes
            .iter()
            .rev()
            .any(|scope| scope.internal_relations.contains(&column.relation())),
        ScalarExpr::Func {
            binding,
            args,
            order_by,
            filter,
            ..
        } => {
            binding
                .as_ref()
                .and_then(crate::semantics::function_implicit_row_input)
                .is_some_and(|column| {
                    expression_has_external_reference(&ScalarExpr::InternalColumn(column), scopes)
                })
                || args
                    .iter()
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
        | ScalarExpr::CompositeRow { items, .. }
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
        | ScalarExpr::Cast { expr: inner, .. }
        | ScalarExpr::Collated { expr: inner, .. } => {
            expression_has_external_reference(inner, scopes)
        }
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
        ScalarExpr::WindowCall {
            args, spec, filter, ..
        } => window_has_external_reference(args, filter.as_deref(), spec, scopes),
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
        | ScalarExpr::TypedLiteral { .. }
        | ScalarExpr::RetainedLiteral(_)
        | ScalarExpr::Param(_)
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. } => false,
    }
}

fn resolves_unqualified(column: &str, scopes: &[QueryScope]) -> bool {
    for scope in scopes.iter().rev() {
        if scope.columns.names.contains(column) || scope.qualifiers.contains_key(column) {
            return true;
        }
        if !scope.columns.complete {
            return false;
        }
    }
    false
}

/// Whether a window call's arguments, `FILTER`, keys or frame offsets refer outside the query scopes.
fn window_has_external_reference(
    args: &[ScalarExpr],
    filter: Option<&ScalarExpr>,
    spec: &crate::ScalarWindowSpec,
    scopes: &[QueryScope],
) -> bool {
    args.iter()
        .chain(filter)
        .chain(&spec.partition_by)
        .chain(spec.order_by.iter().map(|order| &order.expr))
        .any(|expr| expression_has_external_reference(expr, scopes))
        || spec.frame.as_ref().is_some_and(|frame| {
            frame_bound_has_external_reference(&frame.start, scopes)
                || frame_bound_has_external_reference(&frame.end, scopes)
        })
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
mod tests;
