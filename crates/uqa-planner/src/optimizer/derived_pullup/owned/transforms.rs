//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Projection lifting rewrites only local expressions and join conditions, retaining output-relative ordering keys.

use super::{
    MutableQueryPlan, MutableSourcePlan, OwnedScalarExpr, ProductionControl, RelationalPlan,
    RowSchema, SQLError, ScalarExpr, SourceChildSlot, SourcePlan,
};
use uqa_sql::plan::{QueryScalarSlot as Slot, SourceScalarSlot};

type Rewrite<'a> = dyn FnMut(OwnedScalarExpr) -> Result<OwnedScalarExpr, SQLError> + 'a;

pub(super) fn qualify(
    query: MutableQueryPlan,
    schema: &RowSchema,
    control: &ProductionControl<'_>,
) -> Result<(MutableQueryPlan, bool), SQLError> {
    let mut valid = true;
    let query = local(query, control, &mut |expression| {
        expression.replace_nodes(control, &mut |node| {
            let identity = uqa_sql::type_resolution::relation_field_column_ref(node, schema)
                .or_else(|| match node {
                    ScalarExpr::Column(column) => schema
                        .unqualified_position(column)
                        .and_then(|position| schema.identity(position)),
                    ScalarExpr::QualifiedColumn { qualifier, column } => schema
                        .qualified_position(qualifier, column)
                        .and_then(|position| schema.identity(position)),
                    _ => None,
                });
            if let Some(identity) = identity.filter(|identity| identity.qualifier().is_some()) {
                return Ok(Some(super::identity_expression(identity, control)?));
            }
            valid &= !matches!(
                node,
                ScalarExpr::Column(_)
                    | ScalarExpr::QualifiedColumn { .. }
                    | ScalarExpr::Star
                    | ScalarExpr::QualifiedStar(_)
            );
            Ok(None)
        })
    })?;
    Ok((query, valid))
}

pub(super) fn local(
    mut query: MutableQueryPlan,
    control: &ProductionControl<'_>,
    rewrite: &mut Rewrite<'_>,
) -> Result<MutableQueryPlan, SQLError> {
    let RelationalPlan::QueryBlock(block) = &query.root else {
        unreachable!()
    };
    for index in 0..block.projections.len() {
        query = query.map_scalar(Slot::Projection(index), control, &mut *rewrite)?;
    }
    query = query.map_scalar(Slot::Where, control, &mut *rewrite)?;
    let RelationalPlan::QueryBlock(block) = &query.root else {
        unreachable!()
    };
    for index in 0..block.group_by.len() {
        query = query.map_scalar(Slot::GroupBy(index), control, &mut *rewrite)?;
    }
    let RelationalPlan::QueryBlock(block) = &query.root else {
        unreachable!()
    };
    for set in 0..block.grouping_sets.len() {
        let RelationalPlan::QueryBlock(block) = &query.root else {
            unreachable!()
        };
        for expression in 0..block.grouping_sets[set].len() {
            query = query.map_scalar(
                Slot::GroupingSet { set, expression },
                control,
                &mut *rewrite,
            )?;
        }
    }
    for slot in [Slot::Having, Slot::Limit, Slot::Offset] {
        query = query.map_scalar(slot, control, &mut *rewrite)?;
    }
    let RelationalPlan::QueryBlock(block) = &query.root else {
        unreachable!()
    };
    for index in 0..block.order_by.len() {
        let RelationalPlan::QueryBlock(block) = &query.root else {
            unreachable!()
        };
        if !uqa_sql::binding::scoped_references::ordering_uses_output(
            &block.order_by[index].expr,
            &block.projections,
        ) {
            query = query.map_scalar(Slot::OrderBy(index), control, &mut *rewrite)?;
        }
    }
    let RelationalPlan::QueryBlock(block) = &query.root else {
        unreachable!()
    };
    for index in 0..block.distinct_on.len() {
        let RelationalPlan::QueryBlock(block) = &query.root else {
            unreachable!()
        };
        if !uqa_sql::binding::scoped_references::ordering_uses_output(
            &block.distinct_on[index],
            &block.projections,
        ) {
            query = query.map_scalar(Slot::DistinctOn(index), control, &mut *rewrite)?;
        }
    }
    let RelationalPlan::QueryBlock(block) = &query.root else {
        unreachable!()
    };
    for window in 0..block.windows.len() {
        let RelationalPlan::QueryBlock(block) = &query.root else {
            unreachable!()
        };
        for expression in 0..block.windows[window].spec.partition_by.len() {
            query = query.map_scalar(
                Slot::WindowPartition { window, expression },
                control,
                &mut *rewrite,
            )?;
        }
        let RelationalPlan::QueryBlock(block) = &query.root else {
            unreachable!()
        };
        for expression in 0..block.windows[window].spec.order_by.len() {
            query = query.map_scalar(
                Slot::WindowOrder { window, expression },
                control,
                &mut *rewrite,
            )?;
        }
        for slot in [Slot::WindowFrameStart(window), Slot::WindowFrameEnd(window)] {
            query = query.map_scalar(slot, control, &mut *rewrite)?;
        }
    }
    query.map_source(control, |source| joins(source, control, rewrite))
}

fn joins(
    mut source: MutableSourcePlan,
    control: &ProductionControl<'_>,
    rewrite: &mut Rewrite<'_>,
) -> Result<MutableSourcePlan, SQLError> {
    if matches!(&*source, SourcePlan::Join { .. }) {
        source = source.map_child(SourceChildSlot::Left, control, |source| {
            joins(source, control, rewrite)
        })?;
        source = source.map_child(SourceChildSlot::Right, control, |source| {
            joins(source, control, rewrite)
        })?;
        source = source.map_scalar(SourceScalarSlot::JoinOn, control, rewrite)?;
    }
    Ok(source)
}
