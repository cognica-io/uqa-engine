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
    let mut result = Ok(());
    plan.visit_queries_mut(&mut |query| {
        if result.is_ok() {
            result = query_block(query, &context);
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
        || !block.order_by.is_empty()
        || !block.locking.is_empty()
        || !block.privilege_columns.is_empty()
        || input.schema.columns_are_open(None)
        || !block
            .from
            .as_ref()
            .is_some_and(|source| contains_derived_source(source) && ordinary_inner_source(source))
        || block.subqueries.iter().any(subquery_has_column_reference)
    {
        return Ok(());
    }
    let from = block.from.as_ref().expect("source checked");
    let mut lifted = Vec::new();
    candidates(
        from,
        input,
        &from_qualifier_set(from),
        &context,
        &mut lifted,
    )?;
    if lifted.is_empty() {
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
    for candidate in lifted {
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
        changed
            .privilege_columns
            .extend(candidate.privilege_columns);
        if let Some(predicate) = candidate.predicate {
            changed.r#where = Some(match changed.r#where.take() {
                Some(outer) => ScalarExpr::And(vec![filter_acceptance(predicate), outer]),
                None => predicate,
            });
        }
    }
    query.root = RelationalPlan::QueryBlock(changed);
    Ok(())
}

/// A producer's WHERE has already rejected NULL. Scalar AND alone would still evaluate a volatile consumer for that NULL input.
fn filter_acceptance(predicate: ScalarExpr) -> ScalarExpr {
    let binding = uqa_sql::ast::FunctionBinding::dispatched(
        uqa_sql::ast::FunctionDispatch::BooleanTest(uqa_sql::ast::BooleanTest::IsTrue),
    );
    ScalarExpr::Func {
        name: binding.name.clone(),
        binding: Some(binding),
        args: vec![predicate],
        order_syntax: uqa_sql::ast::FunctionOrderSyntax::Ordinary,
        distinct: false,
        order_by: Vec::new(),
        filter: None,
    }
}

struct Lifted {
    qualifier: String,
    source: SourcePlan,
    outputs: BTreeMap<ColumnIdentity, ScalarExpr>,
    predicate: Option<ScalarExpr>,
    privilege_columns: BTreeSet<ColumnIdentity>,
}

fn candidates(
    source: &SourcePlan,
    analyzed: &AnalyzedSource,
    occupied: &BTreeSet<String>,
    context: &Context<'_>,
    output: &mut Vec<Lifted>,
) -> Result<(), SQLError> {
    if let SourcePlan::Join { left, right, .. } = source {
        if let Some(children) = &analyzed.children {
            candidates(left, &children.0, occupied, context, output)?;
            candidates(right, &children.1, occupied, context, output)?;
        }
    } else if let Some(candidate) = candidate(source, analyzed, occupied, context)? {
        // Two independently scoped producers may use the same input alias. Their identities cannot be merged without an opaque input namespace.
        let introduced = from_qualifier_set(&candidate.source);
        if output
            .iter()
            .all(|prior| from_qualifier_set(&prior.source).is_disjoint(&introduced))
        {
            output.push(candidate);
        }
    }
    Ok(())
}

fn candidate(
    source: &SourcePlan,
    analyzed: &AnalyzedSource,
    occupied: &BTreeSet<String>,
    context: &Context<'_>,
) -> Result<Option<Lifted>, SQLError> {
    let SourcePlan::Subquery {
        body,
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
    let Some(from) = &block.from else {
        return Ok(None);
    };
    if from_qualifier_set(from)
        .iter()
        .any(|name| name != qualifier && occupied.contains(name))
    {
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
    let outputs = analyzed
        .schema
        .identities()
        .iter()
        .cloned()
        .zip(
            block
                .projections
                .into_iter()
                .map(|projection| projection.expr),
        )
        .collect();
    let mut privilege_columns = BTreeSet::new();
    for identity in block.privilege_columns {
        let Some(identity) = qualified_identity(&identity, &input.schema) else {
            return Ok(None);
        };
        privilege_columns.insert(identity);
    }
    Ok(Some(Lifted {
        qualifier: qualifier.clone(),
        source: block.from.expect("source checked"),
        outputs,
        predicate: block.r#where,
        privilege_columns,
    }))
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

fn subquery_has_column_reference(query: &QueryPlan) -> bool {
    let mut found = false;
    query.visit_scalar_expressions(&mut |expression| {
        expression.visit(&mut |node| {
            found |= matches!(
                node,
                ScalarExpr::Column(_)
                    | ScalarExpr::QualifiedColumn { .. }
                    | ScalarExpr::QualifiedStar(_)
                    | ScalarExpr::Star
                    | ScalarExpr::InternalColumn(_)
            );
        });
    });
    found
}

fn qualify_block(block: &mut QueryBlockPlan, schema: &RowSchema) -> bool {
    let mut valid = true;
    rewrite_expressions(block, &mut |node| {
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
        .chain(block.order_by.iter_mut().map(|order| &mut order.expr))
        .chain(block.limit.iter_mut())
        .chain(block.offset.iter_mut())
        .chain(&mut block.distinct_on)
    {
        uqa_sql::plan::rewrite_scalar_expression(expression, rewrite);
    }
    for window in &mut block.windows {
        for expression in window.spec.expressions_mut() {
            uqa_sql::plan::rewrite_scalar_expression(expression, rewrite);
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
            uqa_sql::plan::rewrite_scalar_expression(predicate, rewrite);
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
