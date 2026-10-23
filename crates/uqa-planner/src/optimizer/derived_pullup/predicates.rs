//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Move eligible qualifications and their unshared scalar-query arenas together.

use std::collections::BTreeSet;
use uqa_sql::{
    binding::scoped_references::ScopedReferences,
    plan::{QueryPlan, RelationalPlan, SourcePlan},
    semantics::{
        collect_subquery_ids, flatten_and_filter_parts, volatility::scalar_calls_are_volatile,
    },
    ScalarExpr,
};

pub(super) fn push(query: &mut QueryPlan, context: &super::Context<'_>) {
    let Some(Candidate {
        block,
        input,
        qualifier,
        mut producer,
    }) = candidate(query)
    else {
        return;
    };
    let predicate = block.r#where.as_ref().expect("selected qualification");
    let mut moved = Vec::new();
    let mut retained = Vec::new();
    for part in flatten_and_filter_parts(predicate) {
        let simplified = simple_exists(part, &block.subqueries, context);
        let was_simplified = simplified.is_some();
        let mut replacement = simplified.unwrap_or_else(|| part.clone());
        let mut ids = BTreeSet::new();
        collect_subquery_ids(&replacement, &mut ids);
        let eligible = (was_simplified || !ids.is_empty())
            && !scalar_calls_are_volatile(context.volatility, &replacement)
            && ids
                .iter()
                .all(|id| block.subqueries.get(*id).is_some_and(independent));
        replacement.visit_mut(&mut |node| {
            if let Some(identity) = uqa_sql::type_resolution::relation_field_column(node, input) {
                *node = super::identity_expression(&identity);
            }
        });
        let rewritten = eligible
            .then(|| {
                crate::filter_pushdown::derived_output_predicate(
                    context.routines,
                    context.volatility,
                    &producer,
                    qualifier,
                    &replacement,
                    Some(input.columns()),
                    true,
                )
            })
            .flatten();
        if let Some(rewritten) = rewritten {
            moved.push(rewritten);
        } else {
            retained.push(part.clone());
        }
    }
    let Some(mut moved) = crate::filter_pushdown::combine_filter_parts(moved) else {
        return;
    };
    let mut changed = block.clone();
    changed.r#where = crate::filter_pushdown::combine_filter_parts(retained);
    if !uqa_sql::plan::subqueries::transfer_expression(
        &mut moved,
        &mut changed,
        &mut producer.subqueries,
    ) {
        return;
    }
    producer.r#where = Some(match producer.r#where.take() {
        Some(existing) => ScalarExpr::And(vec![existing, moved]),
        None => moved,
    });
    let Some(SourcePlan::Subquery { body, .. }) = &mut changed.from else {
        unreachable!("selected producer")
    };
    body.root = RelationalPlan::QueryBlock(producer);
    query.root = RelationalPlan::QueryBlock(Box::new(changed));
}

/// A source-free, non-aggregating EXISTS observes whether its one possible row passes WHERE. NULL is false, so the qualification is not an ordinary nullable predicate. Negated EXISTS retains PostgreSQL's separate anti-join boundary.
fn simple_exists(
    expression: &ScalarExpr,
    arena: &[QueryPlan],
    context: &super::Context<'_>,
) -> Option<ScalarExpr> {
    use uqa_sql::ast::{BooleanTest, FunctionBinding, FunctionDispatch};
    let ScalarExpr::Exists { subquery, negated } = expression else {
        return None;
    };
    if *negated {
        return None;
    }
    let query = arena.get(*subquery)?;
    let RelationalPlan::QueryBlock(block) = &query.root else {
        return None;
    };
    if !query.ctes.is_empty()
        || block.from.is_some()
        || !matches!(block.compute, uqa_sql::plan::ComputePlan::Project)
        || !block.group_by.is_empty()
        || !block.grouping_sets.is_empty()
        || block.having.is_some()
        || !block.windows.is_empty()
        || block.limit.is_some()
        || block.offset.is_some()
        || !block.locking.is_empty()
        || !block.privilege_columns.is_empty()
        || !block.subqueries.is_empty()
        || block.projections.iter().any(|projection| {
            uqa_sql::semantics::sets::static_setness::expression_may_return_set_statically(
                context.routines,
                &projection.expr,
            )
        })
    {
        return None;
    }
    let predicate = block.r#where.as_ref()?;
    let binding = FunctionBinding::dispatched(FunctionDispatch::BooleanTest(BooleanTest::IsTrue));
    Some(ScalarExpr::Func {
        name: binding.name.clone(),
        binding: Some(binding),
        args: vec![predicate.clone()],
        distinct: false,
        order_by: Vec::new(),
        order_syntax: uqa_sql::ast::FunctionOrderSyntax::default(),
        filter: None,
    })
}

fn independent(query: &QueryPlan) -> bool {
    let mut external = false;
    let mut visit = |node: &ScalarExpr| {
        external |= matches!(
            node,
            ScalarExpr::Column(_)
                | ScalarExpr::QualifiedColumn { .. }
                | ScalarExpr::QualifiedStar(_)
                | ScalarExpr::InternalColumn(_)
        );
    };
    let mut references = ScopedReferences::new(&mut visit);
    references.query(query);
    references.is_complete() && !external
}

struct Candidate<'a> {
    block: &'a uqa_sql::plan::QueryBlockPlan,
    input: &'a uqa_sql::RowSchema,
    qualifier: &'a str,
    producer: Box<uqa_sql::plan::QueryBlockPlan>,
}

fn candidate(query: &QueryPlan) -> Option<Candidate<'_>> {
    let RelationalPlan::QueryBlock(block) = &query.root else {
        return None;
    };
    let predicate = block.r#where.as_ref()?;
    if !uqa_sql::semantics::expr_contains_subquery(predicate)
        || block.subqueries.is_empty()
        || !block.locking.is_empty()
        || !block.privilege_columns.is_empty()
    {
        return None;
    }
    let Some(SourcePlan::Subquery {
        body,
        alias: Some(qualifier),
        lateral: false,
        captured_output: None,
        ..
    }) = &block.from
    else {
        return None;
    };
    if !body.ctes.is_empty() || body.relations_bound != query.relations_bound {
        return None;
    }
    let (Some(input), Some(producer_input)) = (&query.analyzed_source, &body.analyzed_source)
    else {
        return None;
    };
    if input.schema.columns_are_open(None) {
        return None;
    }
    let RelationalPlan::QueryBlock(producer) = &body.root else {
        return None;
    };
    if !producer.locking.is_empty() || !producer.privilege_columns.is_empty() {
        return None;
    }
    let Ok(projections) = uqa_sql::semantics::expand_bound_projection_stars(
        &producer.projections,
        &producer_input.schema,
    ) else {
        return None;
    };
    if projections.len() != input.schema.len() {
        return None;
    }
    let mut producer = producer.clone();
    producer.projections = projections;
    Some(Candidate {
        block,
        input: &input.schema,
        qualifier,
        producer,
    })
}
