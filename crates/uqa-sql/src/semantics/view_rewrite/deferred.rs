//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resolve outer-column substitutions only after the local namespace is known.

use crate::plan::{ExpressionPlan, QueryBlockPlan, QueryPlan, RelationalPlan};
use crate::{ColumnIdentity, RowSchema, SQLError, ScalarExpr};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

mod scope;
mod sources;
pub use sources::{resolve_outer_source_rewrites, resolve_outer_sources};
#[cfg(test)]
mod tests;
pub use scope::bind_outer_rewrite_scope;

/// Resolve an expression against its actual local descriptor. The caller retains the accompanying scalar-query arena for evaluation.
pub fn resolve_outer_expression_rewrites(
    expression: &mut ScalarExpr,
    rewrites: &[DeferredOuterRewrite],
    local: &RowSchema,
    subqueries: &mut Vec<QueryPlan>,
) {
    for rewrite in rewrites {
        rewrite
            .in_namespace(local)
            .expression(expression, subqueries);
    }
}

/// An outer-column rewrite retained across an open native row descriptor. Names in an intervening query scope shadow these substitutions, even when their values are NULL.
#[derive(Debug, Clone)]
pub struct DeferredOuterRewrite {
    outer_columns: Arc<BTreeMap<ColumnIdentity, crate::ast::InternalColumnRef>>,
    replacements: Arc<BTreeMap<ColumnIdentity, OuterReplacement>>,
    pub(super) shadowed_qualifiers: uqa_core::memory::OwnedSet<String>,
    pub(super) shadowed_columns: uqa_core::memory::OwnedSet<String>,
}

#[derive(Debug, Clone)]
struct OuterReplacement {
    expression: ExpressionPlan,
    qualifier: String,
}

/// Bind a pending rewrite to the complete local source descriptor without changing its retained syntax. An open descriptor cannot decide that an absent name belongs to the outer scope.
pub fn resolve_outer_rewrites<'a>(
    block: &'a QueryBlockPlan,
    local: &RowSchema,
) -> Cow<'a, QueryBlockPlan> {
    let open = local.columns_are_open(None);
    if block.deferred_outer_rewrites.is_empty()
        || open
            && !block.deferred_outer_rewrites.iter().any(|rewrite| {
                rewrite
                    .replacements
                    .keys()
                    .any(|identity| identity.qualifier().is_some())
            })
    {
        return Cow::Borrowed(block);
    }
    let mut block = block.clone();
    for mut rewrite in std::mem::take(&mut block.deferred_outer_rewrites) {
        rewrite.add_namespace(local);
        if open {
            let mut pending = rewrite.clone();
            Arc::make_mut(&mut pending.replacements)
                .retain(|identity, _| identity.qualifier().is_none());
            if !pending.replacements.is_empty() {
                block.deferred_outer_rewrites.push(pending);
            }
            Arc::make_mut(&mut rewrite.replacements)
                .retain(|identity, _| identity.qualifier().is_some());
        }
        rewrite.block(&mut block);
    }
    Cow::Owned(block)
}

impl DeferredOuterRewrite {
    pub(crate) fn copy_with(
        &self,
        copying: &mut crate::plan::copy::Copying<'_>,
    ) -> Result<Self, SQLError> {
        Ok(Self {
            outer_columns: Arc::clone(&self.outer_columns),
            replacements: Arc::clone(&self.replacements),
            shadowed_qualifiers: copying.set(&self.shadowed_qualifiers, |copying, name| {
                copying.text(name)
            })?,
            shadowed_columns: copying
                .set(&self.shadowed_columns, |copying, name| copying.text(name))?,
        })
    }

    /// Bind expressions whose original input attributes already have internal identities. Nested query namespaces resolve their own names before consulting these outer replacements.
    pub(crate) fn from_inputs(inputs: BTreeMap<ColumnIdentity, ExpressionPlan>) -> Self {
        Self {
            outer_columns: Arc::new(BTreeMap::new()),
            replacements: Arc::new(
                inputs
                    .into_iter()
                    .map(|(identity, expression)| {
                        (
                            identity,
                            OuterReplacement {
                                expression,
                                qualifier: String::new(),
                            },
                        )
                    })
                    .collect(),
            ),
            shadowed_qualifiers: uqa_core::memory::OwnedSet::new(),
            shadowed_columns: uqa_core::memory::OwnedSet::new(),
        }
    }

    pub(crate) fn bind_expression(&self, expression: &mut ExpressionPlan) {
        self.expression(&mut expression.scalar, &mut expression.subqueries);
    }

    pub(super) fn rebase(
        &mut self,
        context: &super::correlation::CorrelatedRewriteContext<'_>,
    ) -> Result<(), SQLError> {
        for replacement in Arc::make_mut(&mut self.replacements).values_mut() {
            super::correlation::rewrite_outer_expression(context, &mut replacement.expression)?;
        }
        self.capture_outer_columns();
        Ok(())
    }

    pub(super) fn capture(
        context: &super::correlation::CorrelatedRewriteContext<'_>,
        block: &QueryBlockPlan,
        shadowed_qualifiers: &BTreeSet<String>,
        shadowed_columns: &BTreeSet<String>,
    ) -> Result<Self, SQLError> {
        let references = column_references(block);
        let mut replacements = BTreeMap::new();
        for column in &context.layer.columns {
            for qualifier in std::iter::once(None).chain(
                context
                    .target_qualifiers
                    .iter()
                    .map(|qualifier| Some(qualifier.as_str())),
            ) {
                let identity = qualifier.map_or_else(
                    || ColumnIdentity::unqualified(&column.name),
                    |qualifier| ColumnIdentity::qualified(qualifier, &column.name),
                );
                if !references.contains(&identity) {
                    continue;
                }
                let mut subqueries = Vec::new();
                let scalar = super::embed_layer_expression(
                    context.services,
                    &column.expression,
                    context.layer,
                    qualifier.unwrap_or(context.default_target_qualifier),
                    &mut subqueries,
                )?;
                replacements.insert(
                    identity,
                    OuterReplacement {
                        expression: ExpressionPlan { scalar, subqueries },
                        qualifier: qualifier
                            .unwrap_or(context.default_target_qualifier)
                            .to_owned(),
                    },
                );
            }
        }
        if let Some(source) = context.source {
            for column in super::schema_public_columns(source) {
                if !references.contains(&ColumnIdentity::unqualified(&column)) {
                    continue;
                }
                let Some(qualifier) = source
                    .unqualified_position(&column)
                    .and_then(|position| source.identity(position))
                    .and_then(ColumnIdentity::qualifier)
                else {
                    continue;
                };
                replacements
                    .entry(ColumnIdentity::unqualified(&column))
                    .or_insert_with(|| OuterReplacement {
                        expression: ExpressionPlan {
                            scalar: ScalarExpr::qualified_column(qualifier, &column),
                            subqueries: Vec::new(),
                        },
                        qualifier: qualifier.to_owned(),
                    });
            }
        }
        let mut rewrite = Self {
            outer_columns: Arc::new(BTreeMap::new()),
            replacements: Arc::new(replacements),
            shadowed_qualifiers: shadowed_qualifiers.iter().cloned().collect(),
            shadowed_columns: shadowed_columns.iter().cloned().collect(),
        };
        rewrite.capture_outer_columns();
        Ok(rewrite)
    }

    pub(crate) fn expressions(&self) -> impl Iterator<Item = &ExpressionPlan> {
        self.replacements
            .values()
            .map(|replacement| &replacement.expression)
    }

    pub(crate) fn visit_scalar_expressions(&self, visit: &mut dyn FnMut(&ScalarExpr)) {
        for expression in self.expressions() {
            visit(&expression.scalar);
            for query in &expression.subqueries {
                query.visit_scalar_expressions(visit);
            }
        }
    }

    pub(crate) fn rewrite_scalar_expressions(&mut self, rewrite: &mut dyn FnMut(&mut ScalarExpr)) {
        for replacement in Arc::make_mut(&mut self.replacements).values_mut() {
            let expression = &mut replacement.expression;
            crate::plan::rewrite_scalar_expression(&mut expression.scalar, rewrite);
            for query in &mut expression.subqueries {
                query.rewrite_scalar_expressions(rewrite);
            }
        }
    }

    fn add_namespace(&mut self, local: &RowSchema) {
        self.shadowed_columns
            .extend(super::schema_public_columns(local));
        for identity in self.replacements.keys() {
            match identity.qualifier() {
                Some(qualifier) if local.has_qualifier(qualifier) => {
                    self.shadowed_qualifiers.insert(qualifier.to_owned());
                }
                None if local.has_unqualified_column(identity.column())
                    || local.has_qualifier(identity.column()) =>
                {
                    self.shadowed_columns.insert(identity.column().to_owned());
                }
                _ => {}
            }
        }
    }

    fn in_namespace(&self, local: &RowSchema) -> Self {
        let mut rewrite = self.clone();
        rewrite.add_namespace(local);
        if local.columns_are_open(None) {
            Arc::make_mut(&mut rewrite.replacements)
                .retain(|identity, _| identity.qualifier().is_some());
        }
        rewrite
    }

    fn expression(&self, expression: &mut ScalarExpr, subqueries: &mut Vec<QueryPlan>) {
        crate::plan::rewrite_scalar_expression(expression, &mut |node| {
            if let ScalarExpr::ScalarSubquery(id)
            | ScalarExpr::Exists { subquery: id, .. }
            | ScalarExpr::InSubquery { subquery: id, .. } = node
            {
                if let Some(query) = subqueries.get_mut(*id) {
                    self.query(query);
                }
                return;
            }
            let identity = match node {
                ScalarExpr::Column(column) if !self.shadowed_columns.contains(column.as_str()) => {
                    ColumnIdentity::unqualified(column.as_str())
                }
                ScalarExpr::QualifiedStar(qualifier)
                    if !self.shadowed_qualifiers.contains(qualifier.as_str()) =>
                {
                    ColumnIdentity::unqualified(qualifier.as_str())
                }
                ScalarExpr::QualifiedColumn { qualifier, column }
                    if !self.shadowed_qualifiers.contains(qualifier) =>
                {
                    ColumnIdentity::qualified(qualifier.as_str(), column.as_str())
                }
                _ => return,
            };
            let Some(replacement) = self.replacements.get(&identity) else {
                return;
            };
            let offset = subqueries.len();
            let mut replacement = replacement.clone();
            self.bind_replacement_columns(&mut replacement);
            let mut replacement = replacement.expression;
            crate::plan::rewrite_scalar_expression(
                &mut replacement.scalar,
                &mut |node| match node {
                    ScalarExpr::ScalarSubquery(id)
                    | ScalarExpr::Exists { subquery: id, .. }
                    | ScalarExpr::InSubquery { subquery: id, .. } => *id += offset,
                    _ => {}
                },
            );
            subqueries.extend(replacement.subqueries);
            *node = replacement.scalar;
        });
    }

    fn block(&self, block: &mut QueryBlockPlan) {
        for expression in block
            .projections
            .iter_mut()
            .map(|projection| &mut projection.expr)
            .chain(block.r#where.iter_mut())
            .chain(&mut block.group_by)
            .chain(block.grouping_sets.iter_mut().flatten())
            .chain(block.having.iter_mut())
            .chain(block.order_by.iter_mut().map(|order| &mut order.expr))
            .chain(block.limit.iter_mut())
            .chain(block.offset.iter_mut())
            .chain(&mut block.distinct_on)
            .chain(
                block
                    .windows
                    .iter_mut()
                    .flat_map(|window| window.spec.expressions_mut()),
            )
        {
            self.expression(expression, &mut block.subqueries);
        }
    }

    fn query(&self, query: &mut QueryPlan) {
        for cte in &mut query.ctes {
            if let Some(query) = cte.body.query_mut() {
                self.query(query);
            }
        }
        match &mut query.root {
            RelationalPlan::QueryBlock(block) => {
                if let Some(previous) = block
                    .deferred_outer_rewrites
                    .iter_mut()
                    .find(|previous| Arc::ptr_eq(&previous.outer_columns, &self.outer_columns))
                {
                    Arc::make_mut(&mut previous.replacements).extend(
                        self.replacements
                            .iter()
                            .map(|(identity, expression)| (identity.clone(), expression.clone())),
                    );
                    previous
                        .shadowed_columns
                        .extend(self.shadowed_columns.iter().cloned());
                    previous
                        .shadowed_qualifiers
                        .extend(self.shadowed_qualifiers.iter().cloned());
                } else {
                    block.deferred_outer_rewrites.push(self.clone());
                }
            }
            RelationalPlan::Values { rows, subqueries } => {
                for expression in rows.iter_mut().flatten() {
                    self.expression(expression, subqueries);
                }
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
                self.query(left);
                self.query(right);
                for expression in order_by
                    .iter_mut()
                    .map(|order| &mut order.expr)
                    .chain(limit.iter_mut().map(Box::as_mut))
                    .chain(offset.iter_mut().map(Box::as_mut))
                {
                    self.expression(expression, subqueries);
                }
            }
        }
    }
}

fn column_references(block: &QueryBlockPlan) -> BTreeSet<ColumnIdentity> {
    let mut references = BTreeSet::new();
    let mut visit = |expression: &ScalarExpr| {
        expression.visit(&mut |node| match node {
            ScalarExpr::Column(column) => {
                references.insert(ColumnIdentity::unqualified(column));
            }
            ScalarExpr::QualifiedColumn { qualifier, column } => {
                references.insert(ColumnIdentity::qualified(qualifier, column));
            }
            _ => {}
        });
    };
    for expression in block
        .projections
        .iter()
        .map(|projection| &projection.expr)
        .chain(block.r#where.iter())
        .chain(&block.group_by)
        .chain(block.grouping_sets.iter().flatten())
        .chain(block.having.iter())
        .chain(block.order_by.iter().map(|order| &order.expr))
        .chain(block.limit.iter())
        .chain(block.offset.iter())
        .chain(&block.distinct_on)
        .chain(
            block
                .windows
                .iter()
                .flat_map(|window| window.spec.expressions()),
        )
    {
        visit(expression);
    }
    if let Some(source) = &block.from {
        source.visit_scalar_expressions(&mut visit);
    }
    for query in &block.subqueries {
        query.visit_scalar_expressions(&mut visit);
    }
    for rewrite in &block.deferred_outer_rewrites {
        rewrite.visit_scalar_expressions(&mut visit);
    }
    references
}

impl uqa_core::memory::ObserveAllocations for DeferredOuterRewrite {
    fn observe_allocations<'a>(
        &'a self,
        observer: &mut uqa_core::memory::AllocationObserver<'a, '_>,
    ) -> Result<(), uqa_core::ValueRetentionError> {
        let Self {
            outer_columns: _,
            replacements: _,
            shadowed_qualifiers,
            shadowed_columns,
        } = self;
        observer.child(shadowed_qualifiers)?;
        observer.child(shadowed_columns)?;
        Ok(())
    }
}
