//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical-plan correlation analysis for scalar subquery initialization.

#[cfg(test)]
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::plan::{ComputePlan, ProjectionPlan, QueryPlan, RelationalPlan, SourcePlan};
use crate::production::Buffer;
use crate::SQLError;
use crate::ScalarExpr;
use uqa_core::memory::{OwnedMap, OwnedSet, ProductionControl};
mod memory;
use memory::{
    copy_ctes, copy_names, insert, insert_set, Analysis, Ctes, QueryScope, RelationColumns,
};

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
    let context = Analysis {
        context,
        control: ProductionControl::uncontrolled(),
    };
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

    let scope = source_scope(context, source, &Ctes::new(), &[])?;
    if source_references_outer(
        context,
        source,
        &block.subqueries,
        &mut Buffer::new(&context.control),
        &Ctes::new(),
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
        if expression_has_external_reference(
            conjunct,
            std::slice::from_ref(&scope),
            &context.control,
        )? {
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
    query_depends_on_outer_row_with_control(context, plan, &ProductionControl::uncontrolled())
}

pub fn query_depends_on_outer_row_with_control(
    context: CorrelationContext<'_>,
    plan: &QueryPlan,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    query_has_external_reference(
        Analysis {
            context,
            control: *control,
        },
        plan,
        &mut Buffer::new(control),
    )
}

/// Whether an expression needs a row outside its FROM namespace; nested subqueries retain their local aliases and CTEs.
pub fn expression_depends_on_outer_row(
    context: CorrelationContext<'_>,
    expression: &crate::plan::ExpressionPlan,
    source: Option<&SourcePlan>,
    definitions: &[crate::plan::CtePlan],
) -> Result<bool, SQLError> {
    expression_depends_on_outer_row_with_control(
        context,
        &expression.scalar,
        &expression.subqueries,
        source,
        definitions,
        &ProductionControl::uncontrolled(),
    )
}

/// Borrow the scalar and its original query arena while admitting lexical namespace scratch before allocation.
pub fn expression_depends_on_outer_row_with_control(
    context: CorrelationContext<'_>,
    scalar: &ScalarExpr,
    subqueries: &[QueryPlan],
    source: Option<&SourcePlan>,
    definitions: &[crate::plan::CtePlan],
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    control.check()?;
    let context = Analysis {
        context,
        control: *control,
    };
    let mut ctes = Ctes::new();
    for cte in definitions.iter().filter(|cte| cte.recursive) {
        let mut columns = RelationColumns::known(
            copy_names(cte.columns.iter().map(String::as_str), control)?,
            control,
        )?;
        columns.complete = false;
        insert(
            &mut ctes,
            control.copy_text(&cte.name)?,
            columns.share(control)?,
            control,
        )?;
    }
    for cte in definitions {
        let columns = cte_output_columns(context, cte, &ctes, &[])?;
        insert(
            &mut ctes,
            control.copy_text(&cte.name)?,
            columns.share(control)?,
            control,
        )?;
    }
    let mut scopes = Buffer::new(control);
    if let Some(source) = source {
        scopes.push(source_scope(context, source, &ctes, &[])?, control)?;
    }
    expression_references_outer(context, scalar, subqueries, &mut scopes, &ctes)
}

/// Enclosing identities resolve beyond the calling row rather than becoming level-zero dependencies.
pub(crate) fn query_depends_on_current_row(
    context: CorrelationContext<'_>,
    plan: &QueryPlan,
    input: &crate::RowSchema,
) -> Result<bool, SQLError> {
    let control = ProductionControl::uncontrolled();
    let context = Analysis { context, control };
    let mut scopes = Buffer::new(&control);
    scopes.push(enclosing_scope(input, &control)?, &control)?;
    query_has_external_reference(context, plan, &mut scopes)
}

fn enclosing_scope(
    input: &crate::RowSchema,
    control: &ProductionControl<'_>,
) -> Result<QueryScope, SQLError> {
    let deferred_columns = RelationColumns::empty(false, control)?.share(control)?;
    let mut columns = RelationColumns::empty(true, control)?;
    let mut qualifiers = OwnedMap::new();
    let mut internal_relations = OwnedSet::new();
    for (identity, _) in input.typed_physical_alias_identities() {
        control.check()?;
        if !input.resolves_local_column(identity.qualifier(), identity.column()) {
            if let Some(qualifier) = identity.qualifier() {
                if !qualifiers.contains_key(qualifier) {
                    insert(
                        &mut qualifiers,
                        control.copy_text(qualifier)?,
                        Arc::clone(&deferred_columns),
                        control,
                    )?;
                }
            } else {
                insert_set(
                    &mut columns.names,
                    control.copy_text(identity.column())?,
                    control,
                )?;
            }
        }
    }
    for (column, slot) in input.internal_identities() {
        if !input.slot_is_local(slot) {
            insert_set(&mut internal_relations, column.relation(), control)?;
        }
    }
    Ok(QueryScope {
        columns: columns.share(control)?,
        qualifiers,
        internal_relations,
        enclosing: Some(input.clone()),
    })
}

fn query_has_external_reference(
    context: Analysis<'_, '_>,
    plan: &QueryPlan,
    scopes: &mut Buffer<QueryScope>,
) -> Result<bool, SQLError> {
    query_references_outer(context, plan, scopes, &Ctes::new())
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves scope and subquery identity"
)]
fn query_references_outer(
    context: Analysis<'_, '_>,
    plan: &QueryPlan,
    scopes: &mut Buffer<QueryScope>,
    parent_ctes: &Ctes,
) -> Result<bool, SQLError> {
    context.control.check()?;
    let control = &context.control;
    let mut ctes = copy_ctes(parent_ctes, control)?;
    if plan.ctes.iter().any(|cte| cte.recursive) {
        for cte in &plan.ctes {
            let mut columns = RelationColumns::known(
                copy_names(cte.columns.iter().map(String::as_str), control)?,
                control,
            )?;
            columns.complete = false;
            insert(
                &mut ctes,
                control.copy_text(&cte.name)?,
                columns.share(control)?,
                control,
            )?;
        }
    }
    for cte in &plan.ctes {
        let columns = cte_output_columns(context, cte, &ctes, scopes)?.share(control)?;
        if cte.recursive {
            insert(
                &mut ctes,
                control.copy_text(&cte.name)?,
                Arc::clone(&columns),
                control,
            )?;
        }
        if cte.body.query().map_or(Ok(true), |query| {
            query_references_outer(context, query, scopes, &ctes)
        })? {
            return Ok(true);
        }
        insert(&mut ctes, control.copy_text(&cte.name)?, columns, control)?;
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
                None => QueryScope::empty(control)?,
            };
            scopes.push(scope, control)?;
            let result = block
                .try_visit_expressions(&mut |expression| {
                    expression_references_outer(
                        context,
                        expression,
                        &block.subqueries,
                        scopes,
                        &ctes,
                    )
                    .map(|references_outer| !references_outer)
                })
                .map(|completed| !completed);
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
            let scope = QueryScope::relation(
                None,
                query_output_columns(context, left, &ctes, scopes)?,
                control,
            )?;
            scopes.push(scope, control)?;
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
            scopes.push(QueryScope::empty(control)?, control)?;
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

fn expression_has_external_reference(
    expression: &ScalarExpr,
    scopes: &[QueryScope],
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    let mut found = false;
    expression.try_visit(&mut |node| {
        control.check()?;
        if found {
            return Ok(false);
        }
        found = match node {
            ScalarExpr::Column(column) => !resolves_unqualified(column, scopes),
            ScalarExpr::QualifiedColumn { qualifier, .. } => !scopes
                .iter()
                .rev()
                .any(|scope| scope.qualifiers.contains_key(qualifier.as_str())),
            ScalarExpr::QualifiedStar(qualifier) => !scopes
                .iter()
                .rev()
                .any(|scope| scope.qualifiers.contains_key(qualifier.as_str())),
            ScalarExpr::InternalColumn(column) => !scopes
                .iter()
                .rev()
                .any(|scope| scope.internal_relations.contains(&column.relation())),
            ScalarExpr::Func { binding, .. } => binding
                .as_ref()
                .and_then(crate::semantics::function_implicit_row_input)
                .is_some_and(|column| {
                    !scopes
                        .iter()
                        .rev()
                        .any(|scope| scope.internal_relations.contains(&column.relation()))
                }),
            _ => false,
        };
        Ok::<_, SQLError>(!found)
    })?;
    Ok(found)
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

#[cfg(test)]
mod tests;
