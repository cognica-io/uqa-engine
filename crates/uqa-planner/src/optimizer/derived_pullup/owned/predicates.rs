//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Qualification movement preserves exclusive scalar-query ownership and the original rollback boundary.

use super::{
    Buffer, Context, MutableQueryPlan, OwnedScalarExpr, ProductionControl, RelationalPlan,
    SQLError, ScalarExpr, SourcePlan,
};
use uqa_core::memory::ProductionMap;
use uqa_sql::plan::{ComputePlan, QueryPlan};

pub(super) fn push(
    query: MutableQueryPlan,
    context: &Context<'_>,
    control: &ProductionControl<'_>,
) -> Result<MutableQueryPlan, SQLError> {
    control.check()?;
    let RelationalPlan::QueryBlock(block) = &query.root else {
        return Ok(query);
    };
    let Some(predicate) = &block.r#where else {
        return Ok(query);
    };
    if !uqa_sql::semantics::expr_contains_subquery(predicate)
        || block.subqueries.is_empty()
        || !block.locking.is_empty()
        || !block.privilege_columns.is_empty()
    {
        return Ok(query);
    }
    let Some(SourcePlan::Subquery {
        body,
        alias: Some(qualifier),
        lateral: false,
        captured_output: None,
        ..
    }) = &block.from
    else {
        return Ok(query);
    };
    if !body.ctes.is_empty()
        || body.relations_bound != query.relations_bound
        || body.relation_privilege_subject.is_some()
    {
        return Ok(query);
    }
    let (Some(input), Some(_)) = (&query.analyzed_source, &body.analyzed_source) else {
        return Ok(query);
    };
    if input.schema.columns_are_open(None) {
        return Ok(query);
    }
    let RelationalPlan::QueryBlock(producer) = &body.root else {
        return Ok(query);
    };
    if !producer.locking.is_empty() || !producer.privilege_columns.is_empty() {
        return Ok(query);
    }
    let producer = body.copy_for_rewrite(control)?;
    let (producer, expanded) = super::super::super::derived_projection::owned::expand_stars(
        producer,
        Some(&input.schema),
        control,
    )?;
    if !expanded || super::projections(&producer).len() != input.schema.len() {
        return Ok(query);
    }
    let changed = query.copy_for_rewrite(control)?;
    let (changed, predicate) = changed.take_where(control)?;
    let mut parts = Buffer::new(*control);
    super::flatten(
        predicate.expect("selected qualification"),
        &mut parts,
        control,
    )?;
    let mut moved = Buffer::new(*control);
    let mut retained = Buffer::new(*control);
    for part in parts {
        let (replacement, eligible) = pushable_part(&part, &block.subqueries, context, control)?;
        let replacement = replacement.replace_nodes(control, &mut |node| {
            uqa_sql::type_resolution::relation_field_column_ref(node, &input.schema)
                .map(|identity| super::identity_expression(identity, control))
                .transpose()
        })?;
        let rewritten = if eligible {
            let RelationalPlan::QueryBlock(producer) = &producer.root else {
                unreachable!()
            };
            crate::filter_pushdown::owned::derived_output_predicate(
                crate::filter_pushdown::owned::PredicateServices {
                    routines: context.routines,
                    volatility: context.volatility,
                },
                producer,
                qualifier,
                &replacement,
                Some(input.schema.columns()),
                true,
                control,
            )?
        } else {
            None
        };
        if let Some(rewritten) = rewritten {
            moved.push(rewritten)?;
        } else {
            retained.push(part)?;
        }
    }
    transfer(query, changed, producer, moved, retained, control)
}

fn transfer(
    query: MutableQueryPlan,
    changed: MutableQueryPlan,
    producer: MutableQueryPlan,
    moved: Buffer<OwnedScalarExpr>,
    retained: Buffer<OwnedScalarExpr>,
    control: &ProductionControl<'_>,
) -> Result<MutableQueryPlan, SQLError> {
    let Some(moved) = super::combine(moved, control)? else {
        return Ok(query);
    };
    let changed = changed.replace_where(super::combine(retained, control)?, control)?;
    let (changed, producer, moved, transferred) =
        changed.transfer_expression_to(producer, moved, control)?;
    if !transferred {
        return Ok(query);
    }
    let producer = super::append_where(producer, moved, control)?;
    let changed = changed.map_source(control, |source| {
        source.map_query(control, |_| Ok(producer))
    })?;
    drop(query);
    Ok(changed)
}

fn pushable_part(
    part: &OwnedScalarExpr,
    subqueries: &[QueryPlan],
    context: &Context<'_>,
    control: &ProductionControl<'_>,
) -> Result<(OwnedScalarExpr, bool), SQLError> {
    let simplified = simple_exists(part, subqueries, context, control)?;
    let was_simplified = simplified.is_some();
    let replacement = match simplified {
        Some(value) => value,
        None => part.copy_for_rewrite(control)?,
    };
    let mut ids = ProductionMap::new(*control);
    replacement.try_visit(&mut |node| {
        control.check()?;
        match node {
            ScalarExpr::ScalarSubquery(id)
            | ScalarExpr::Exists { subquery: id, .. }
            | ScalarExpr::InSubquery { subquery: id, .. } => {
                ids.insert_copy(*id, ())?;
            }
            _ => {}
        }
        Ok::<_, SQLError>(true)
    })?;
    let mut eligible = (was_simplified || !ids.is_empty())
        && !uqa_sql::semantics::volatility::scalar_calls_are_volatile_with_control(
            context.volatility,
            &replacement,
            control,
        )?;
    if eligible {
        for (id, ()) in &ids {
            let Some(query) = subqueries.get(*id) else {
                eligible = false;
                break;
            };
            if !independent(query, control)? {
                eligible = false;
                break;
            }
        }
    }
    Ok((replacement, eligible))
}

fn simple_exists(
    expression: &ScalarExpr,
    arena: &[QueryPlan],
    context: &Context<'_>,
    control: &ProductionControl<'_>,
) -> Result<Option<OwnedScalarExpr>, SQLError> {
    use uqa_sql::ast::{BooleanTest, FunctionBinding, FunctionDispatch};
    let ScalarExpr::Exists {
        subquery,
        negated: false,
    } = expression
    else {
        return Ok(None);
    };
    let Some(query) = arena.get(*subquery) else {
        return Ok(None);
    };
    let RelationalPlan::QueryBlock(block) = &query.root else {
        return Ok(None);
    };
    if !query.ctes.is_empty()
        || block.from.is_some()
        || !matches!(block.compute, ComputePlan::Project)
        || !block.group_by.is_empty()
        || !block.grouping_sets.is_empty()
        || block.having.is_some()
        || !block.windows.is_empty()
        || block.limit.is_some()
        || block.offset.is_some()
        || !block.locking.is_empty()
        || !block.privilege_columns.is_empty()
        || !block.subqueries.is_empty()
    {
        return Ok(None);
    }
    for projection in &block.projections {
        if uqa_sql::semantics::sets::static_setness::expression_may_return_set_statically_with_control(context.routines, &projection.expr, control)? {
            return Ok(None);
        }
    }
    let Some(predicate) = &block.r#where else {
        return Ok(None);
    };
    let binding = FunctionBinding::dispatched_with_control(
        FunctionDispatch::BooleanTest(BooleanTest::IsTrue),
        control,
    )?;
    Ok(Some(OwnedScalarExpr::call(
        control.copy_text(&binding.name)?,
        Some(binding),
        std::iter::once(predicate.copy_for_rewrite(control)?),
        control,
    )?))
}

fn independent(query: &QueryPlan, control: &ProductionControl<'_>) -> Result<bool, SQLError> {
    let mut external = false;
    let mut visit = |node: &ScalarExpr| {
        control.check()?;
        external |= matches!(
            node,
            ScalarExpr::Column(_)
                | ScalarExpr::QualifiedColumn { .. }
                | ScalarExpr::QualifiedStar(_)
                | ScalarExpr::InternalColumn(_)
        );
        Ok(())
    };
    let mut references =
        uqa_sql::binding::scoped_references::ControlledScopedReferences::new(&mut visit, *control);
    references.query(query)?;
    Ok(references.is_complete() && !external)
}
