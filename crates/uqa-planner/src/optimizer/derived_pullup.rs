//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lift pure derived projections into their consumer before its qualification runs.

use std::collections::{BTreeMap, BTreeSet};

use uqa_sql::{
    ast::JoinKind,
    plan::{
        AnalyzedSource, ComputePlan, QueryBlockPlan, QueryPlan, RelationalPlan, SourcePlan,
        UnifiedPlan,
    },
    semantics::{from_qualifier_set, volatility::query_contains_volatile_function},
    ColumnIdentity, RowSchema, SQLError, ScalarExpr,
};

use super::OptimizerConfig;

mod predicates;

pub(super) fn rewrite(
    plan: &mut UnifiedPlan,
    config: &OptimizerConfig<'_>,
) -> Result<(), SQLError> {
    let Some(catalog) = &config.routine_inlining else {
        return Ok(());
    };
    let context = Context {
        routines: catalog.routines,
        volatility: catalog.volatility,
        relations_bound: false,
    };
    rewrite_plan(plan, &context)
}

fn rewrite_plan(plan: &mut UnifiedPlan, context: &Context<'_>) -> Result<(), SQLError> {
    if let UnifiedPlan::Command(command) = plan {
        match command.as_mut() {
            uqa_sql::plan::CommandPlan::CreateView { .. }
            | uqa_sql::plan::CommandPlan::CreateMaterializedView { .. }
            | uqa_sql::plan::CommandPlan::Prepare { .. } => return Ok(()),
            uqa_sql::plan::CommandPlan::Explain { .. } => return Ok(()),
            _ => {}
        }
    }
    let mut result = Ok(());
    plan.visit_queries_mut(&mut |query| {
        if result.is_ok() {
            result = query_block(query, context);
            if result.is_ok() {
                predicates::push(query, context);
            }
        }
    });
    result
}

#[derive(Clone, Copy)]
struct Context<'a> {
    routines: &'a dyn uqa_sql::routines::RoutineResolution,
    volatility: &'a dyn uqa_sql::semantics::volatility::VolatilityCatalog,
    relations_bound: bool,
}

fn query_block(query: &mut QueryPlan, context: &Context<'_>) -> Result<(), SQLError> {
    let context = Context {
        relations_bound: query.relations_bound,
        ..*context
    };
    let (Some(input), Some(output)) = (&query.analyzed_source, &query.analyzed_output) else {
        return Ok(());
    };
    let RelationalPlan::QueryBlock(block) = &query.root else {
        return Ok(());
    };
    if block.r#where.is_none()
        || !block.locking.is_empty()
        || !block.privilege_columns.is_empty()
        || input.schema.columns_are_open(None)
        || !block
            .from
            .as_ref()
            .is_some_and(|source| contains_derived_source(source) && ordinary_inner_source(source))
    {
        return Ok(());
    }
    let mut changed = block.clone();
    let Ok(projections) =
        uqa_sql::semantics::expand_bound_projection_stars(&changed.projections, &input.schema)
    else {
        return Ok(());
    };
    if projections.len() != output.len() {
        return Ok(());
    }
    changed.projections = projections;
    if !qualify_block(&mut changed, &input.schema) {
        return Ok(());
    }
    for (projection, label) in changed.projections.iter_mut().zip(output.columns()) {
        projection.alias = Some(label.clone());
    }
    let mut predicates = changed
        .r#where
        .as_ref()
        .map(uqa_sql::semantics::flatten_and_filter_parts)
        .unwrap_or_default()
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
    let mut lifted = Vec::new();
    candidates(
        changed.from.as_ref().expect("source checked"),
        input,
        &context,
        &mut predicates,
        &mut lifted,
    )?;
    if lifted.is_empty() {
        return Ok(());
    }
    changed.r#where = crate::filter_pushdown::combine_filter_parts(predicates);
    for candidate in lifted {
        for subquery in &mut changed.subqueries {
            if !uqa_sql::binding::scoped_references::substitute_outer_columns(
                subquery,
                &input.schema,
                &candidate.outputs,
            ) {
                return Ok(());
            }
        }
        rewrite_expressions(&mut changed, &mut |node| {
            let identity = column_identity(node);
            if let Some(expression) = identity.and_then(|identity| candidate.outputs.get(&identity))
            {
                *node = expression.clone();
            }
        });
        replace_source(
            changed.from.as_mut().expect("source"),
            &candidate.qualifier,
            candidate.source,
        );
    }
    query.root = RelationalPlan::QueryBlock(changed);
    Ok(())
}

struct Lifted {
    qualifier: String,
    source: SourcePlan,
    outputs: BTreeMap<ColumnIdentity, ScalarExpr>,
}

fn candidates(
    source: &SourcePlan,
    analyzed: &AnalyzedSource,
    context: &Context<'_>,
    predicates: &mut Vec<ScalarExpr>,
    output: &mut Vec<Lifted>,
) -> Result<(), SQLError> {
    if let SourcePlan::Join { left, right, .. } = source {
        if let Some(children) = &analyzed.children {
            candidates(left, &children.0, context, predicates, output)?;
            candidates(right, &children.1, context, predicates, output)?;
        }
    } else if let Some(candidate) = candidate(source, analyzed, context, predicates)? {
        output.push(candidate);
    }
    Ok(())
}

fn candidate(
    source: &SourcePlan,
    analyzed: &AnalyzedSource,
    context: &Context<'_>,
    predicates: &mut Vec<ScalarExpr>,
) -> Result<Option<Lifted>, SQLError> {
    let SourcePlan::Subquery {
        body,
        captured_output: None,
        alias: Some(qualifier),
        lateral: false,
        ..
    } = source
    else {
        return Ok(None);
    };
    let Some(input) = &body.analyzed_source else {
        return Ok(None);
    };
    let RelationalPlan::QueryBlock(block) = &body.root else {
        return Ok(None);
    };
    if body.relations_bound != context.relations_bound
        || body.relation_privilege_subject.is_some()
        || !body.ctes.is_empty()
        || !block.subqueries.is_empty()
        || !matches!(block.compute, ComputePlan::Project)
        || block.distinct
        || !block.distinct_on.is_empty()
        || !block.group_by.is_empty()
        || !block.grouping_sets.is_empty()
        || block.having.is_some()
        || !block.windows.is_empty()
        || !block.order_by.is_empty()
        || block.limit.is_some()
        || block.offset.is_some()
        || !block.locking.is_empty()
        || query_contains_volatile_function(context.volatility, body)?
        || block.projections.iter().any(|projection| {
            uqa_sql::semantics::sets::static_setness::expression_may_return_set_statically(
                context.routines,
                &projection.expr,
            )
        })
    {
        return Ok(None);
    }
    if block.from.is_none() || input.schema.columns_are_open(None) {
        return Ok(None);
    }
    let mut block = block.clone();
    let Ok(projections) =
        uqa_sql::semantics::expand_bound_projection_stars(&block.projections, &input.schema)
    else {
        return Ok(None);
    };
    if projections.len() != analyzed.schema.len()
        || analyzed
            .schema
            .identities()
            .iter()
            .collect::<BTreeSet<_>>()
            .len()
            != projections.len()
    {
        return Ok(None);
    }
    block.projections = projections;
    if !qualify_block(&mut block, &input.schema) {
        return Ok(None);
    }
    let Some(capture) = capture_expressions(&block, &input.schema, &analyzed.schema) else {
        return Ok(None);
    };
    move_scan_predicates(
        &mut block,
        qualifier,
        analyzed.schema.columns(),
        context,
        predicates,
    );
    let source = capture.source(body, block, &input.schema, qualifier);
    Ok(Some(Lifted {
        qualifier: qualifier.clone(),
        source,
        outputs: capture.outputs,
    }))
}

struct CapturedOutput {
    relation: uqa_sql::ast::InternalRelationId,
    captures: Vec<usize>,
    outputs: BTreeMap<ColumnIdentity, ScalarExpr>,
}

impl CapturedOutput {
    // Keep producer names, authorization and qualification in their original scope. Only demanded raw attributes cross its output boundary.
    fn source(
        &self,
        body: &QueryPlan,
        mut block: Box<QueryBlockPlan>,
        input: &RowSchema,
        qualifier: &str,
    ) -> SourcePlan {
        block.projections = self
            .captures
            .iter()
            .map(|position| {
                let identity = &input.identities()[*position];
                uqa_sql::plan::ProjectionPlan {
                    expr: identity_expression(identity),
                    alias: Some(identity.column().to_owned()),
                }
            })
            .collect();
        let columns = self
            .captures
            .iter()
            .map(|position| (input.columns()[*position].clone(), *position))
            .collect::<Vec<_>>();
        let descriptor = RowSchema::remap_positions(input, &columns, &[]);
        let mut captured = body.clone();
        captured.analyzed_output = Some(descriptor.for_result_columns(descriptor.columns()));
        captured.root = RelationalPlan::QueryBlock(block);
        SourcePlan::Subquery {
            captured_output: Some(self.relation),
            lateral: false,
            body: Box::new(captured),
            alias: Some(qualifier.to_owned()),
            column_aliases: Vec::new(),
        }
    }
}

fn capture_expressions(
    block: &QueryBlockPlan,
    input: &RowSchema,
    output: &RowSchema,
) -> Option<CapturedOutput> {
    let relation = uqa_sql::ast::InternalRelationId::allocate();
    let mut outputs = BTreeMap::new();
    let mut captures = Vec::new();
    let mut attributes = BTreeMap::new();
    for (identity, projection) in output.identities().iter().zip(&block.projections) {
        let mut expression = projection.expr.clone();
        let mut valid = true;
        uqa_sql::plan::rewrite_scalar_expression(&mut expression, &mut |node| {
            if let Some(identity) = column_identity(node) {
                let position = input.qualified_position(
                    identity.qualifier().expect("qualified input"),
                    identity.column(),
                );
                if let Some(position) = position {
                    let attribute = *attributes.entry(position).or_insert_with(|| {
                        captures.push(position);
                        captures.len() - 1
                    });
                    *node = ScalarExpr::InternalColumn(relation.column(attribute));
                } else {
                    valid = false;
                }
            } else if matches!(node, ScalarExpr::InternalColumn(_)) {
                valid = false;
            }
        });
        if !valid {
            return None;
        }
        outputs.insert(identity.clone(), expression);
    }
    Some(CapturedOutput {
        relation,
        captures,
        outputs,
    })
}

fn move_scan_predicates(
    block: &mut QueryBlockPlan,
    qualifier: &str,
    columns: &[String],
    context: &Context<'_>,
    predicates: &mut Vec<ScalarExpr>,
) {
    // A pure qualification with only this producer's outputs belongs at its scan. Move, rather than duplicate, it so existing indexes and stable-call counts keep their original boundary.
    let mut retained = Vec::new();
    for predicate in std::mem::take(predicates) {
        let mut used = BTreeSet::new();
        let rewritten = if !uqa_sql::semantics::expr_contains_subquery(&predicate)
            && !uqa_sql::semantics::volatility::expr_contains_volatile_function(
                context.volatility,
                &predicate,
            ) {
            crate::filter_pushdown::rewrite_output_filter(
                &predicate,
                qualifier,
                columns,
                &block.projections,
                &mut used,
            )
        } else {
            None
        };
        if let Some(rewritten) = rewritten.filter(|_| !used.is_empty()) {
            block.r#where = Some(match block.r#where.take() {
                Some(existing) => ScalarExpr::And(vec![existing, rewritten]),
                None => rewritten,
            });
        } else {
            retained.push(predicate);
        }
    }
    *predicates = retained;
}

fn contains_derived_source(source: &SourcePlan) -> bool {
    match source {
        SourcePlan::Subquery { .. } => true,
        SourcePlan::Join { left, right, .. } => {
            contains_derived_source(left) || contains_derived_source(right)
        }
        _ => false,
    }
}

fn ordinary_inner_source(source: &SourcePlan) -> bool {
    match source {
        SourcePlan::Join {
            left,
            right,
            kind: JoinKind::Inner | JoinKind::Cross,
            alias: None,
            using: None,
            natural: false,
            lateral: false,
            ..
        } => ordinary_inner_source(left) && ordinary_inner_source(right),
        SourcePlan::Table { .. }
        | SourcePlan::Values { lateral: false, .. }
        | SourcePlan::Subquery { lateral: false, .. } => true,
        _ => false,
    }
}

fn qualify_block(block: &mut QueryBlockPlan, schema: &RowSchema) -> bool {
    let mut valid = true;
    rewrite_expressions(block, &mut |node| {
        if let Some(identity) = uqa_sql::type_resolution::relation_field_column(node, schema) {
            *node = identity_expression(&identity);
        }
        if let Some(identity) = column_identity(node) {
            if let Some(identity) = qualified_identity(&identity, schema) {
                *node = identity_expression(&identity);
            } else {
                valid = false;
            }
        } else if matches!(node, ScalarExpr::QualifiedStar(_) | ScalarExpr::Star) {
            valid = false;
        }
    });
    valid
}

fn qualified_identity(identity: &ColumnIdentity, schema: &RowSchema) -> Option<ColumnIdentity> {
    let position = match identity.qualifier() {
        Some(qualifier) => schema.qualified_position(qualifier, identity.column()),
        None => schema.unqualified_position(identity.column()),
    }?;
    let identity = schema.identity(position)?;
    identity.qualifier().map(|_| identity.clone())
}

fn column_identity(expression: &ScalarExpr) -> Option<ColumnIdentity> {
    match expression {
        ScalarExpr::Column(column) => Some(ColumnIdentity::unqualified(column.clone())),
        ScalarExpr::QualifiedColumn { qualifier, column } => {
            Some(ColumnIdentity::qualified(qualifier.clone(), column.clone()))
        }
        _ => None,
    }
}

fn identity_expression(identity: &ColumnIdentity) -> ScalarExpr {
    match identity.qualifier() {
        Some(qualifier) => ScalarExpr::QualifiedColumn {
            qualifier: qualifier.into(),
            column: identity.column().into(),
        },
        None => ScalarExpr::Column(identity.column().into()),
    }
}

fn rewrite_expressions(block: &mut QueryBlockPlan, rewrite: &mut dyn FnMut(&mut ScalarExpr)) {
    for expression in block
        .projections
        .iter_mut()
        .map(|projection| &mut projection.expr)
        .chain(block.r#where.iter_mut())
        .chain(&mut block.group_by)
        .chain(block.grouping_sets.iter_mut().flatten())
        .chain(block.having.iter_mut())
        .chain(block.limit.iter_mut())
        .chain(block.offset.iter_mut())
    {
        expression.visit_mut(&mut |node| rewrite(node));
    }
    for expression in block
        .order_by
        .iter_mut()
        .map(|order| &mut order.expr)
        .chain(&mut block.distinct_on)
    {
        if !uqa_sql::binding::scoped_references::ordering_uses_output(
            expression,
            &block.projections,
        ) {
            expression.visit_mut(&mut |node| rewrite(node));
        }
    }
    for window in &mut block.windows {
        for expression in window.spec.expressions_mut() {
            expression.visit_mut(&mut |node| rewrite(node));
        }
    }
    if let Some(source) = &mut block.from {
        rewrite_join_conditions(source, rewrite);
    }
}

fn rewrite_join_conditions(source: &mut SourcePlan, rewrite: &mut dyn FnMut(&mut ScalarExpr)) {
    if let SourcePlan::Join {
        left, right, on, ..
    } = source
    {
        rewrite_join_conditions(left, rewrite);
        rewrite_join_conditions(right, rewrite);
        if let Some(predicate) = on {
            predicate.visit_mut(&mut |node| rewrite(node));
        }
    }
}

fn replace_source(source: &mut SourcePlan, qualifier: &str, replacement: SourcePlan) {
    match source {
        SourcePlan::Subquery {
            alias: Some(alias), ..
        } if alias == qualifier => *source = replacement,
        SourcePlan::Join { left, right, .. } => {
            if from_qualifier_set(left).contains(qualifier) {
                replace_source(left, qualifier, replacement);
            } else {
                replace_source(right, qualifier, replacement);
            }
        }
        _ => unreachable!("selected derived source remains in its input tree"),
    }
}

#[cfg(test)]
mod tests;
