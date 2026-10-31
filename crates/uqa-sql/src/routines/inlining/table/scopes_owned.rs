//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Caller attributes and result coercions move through independent scalar and source constructors.

use crate::{
    ast::InternalRelationId,
    plan::{
        MutableQueryPlan, MutableSourcePlan, OwnedProjection, OwnedScalarExpr, QueryScalarSlot,
        RelationalPlan,
    },
    ColumnType, SQLError, ScalarExpr,
};
use uqa_core::memory::{MemoryReservation, Produced, ProductionControl, ProductionVec};

pub(super) struct Items<T> {
    values: std::vec::IntoIter<T>,
    _memory: Option<MemoryReservation>,
}

impl<T> Items<T> {
    pub(super) fn new(values: Produced<Vec<T>>) -> Self {
        let (values, memory) = values.into_parts();
        Self {
            values: values.into_iter(),
            _memory: memory,
        }
    }
}

impl<T> Iterator for Items<T> {
    type Item = T;
    fn next(&mut self) -> Option<T> {
        self.values.next()
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.values.size_hint()
    }
}
impl<T> ExactSizeIterator for Items<T> {}

pub(super) fn substitute(
    query: MutableQueryPlan,
    arguments: &[ScalarExpr],
    width: usize,
    control: &ProductionControl<'_>,
) -> Result<MutableQueryPlan, SQLError> {
    let relation = InternalRelationId::allocate();
    let mut captures = ProductionVec::<OwnedScalarExpr>::new(*control);
    let query = query.transform_scalar_nodes(control, &mut |_, node| {
        let ScalarExpr::Param(index) = &*node else {
            return Ok(node);
        };
        let argument = index
            .checked_sub(1)
            .and_then(|index| arguments.get(index))
            .ok_or_else(|| {
                SQLError::Internal("SQL body parameter is outside its invocation".into())
            })?;
        argument
            .copy_for_rewrite(control)?
            .transform_nodes(control, &mut |_, reference| {
                if !matches!(
                    &*reference,
                    ScalarExpr::Column(_)
                        | ScalarExpr::QualifiedColumn { .. }
                        | ScalarExpr::QualifiedStar(_)
                        | ScalarExpr::Position(_)
                ) {
                    return Ok(reference);
                }
                let mut found = None;
                for (index, capture) in captures.iter().enumerate() {
                    if capture.has_same_syntax_with_control(&reference, control)? {
                        found = Some(index);
                        break;
                    }
                }
                let position = if let Some(index) = found {
                    index
                } else {
                    let index = captures.len();
                    captures
                        .push_produced(control.finish(reference, control.empty_reservation())?)?;
                    index
                };
                Ok(ScalarExpr::InternalColumn(relation.column(position))
                    .copy_for_rewrite(control)?)
            })
    })?;
    if captures.is_empty() {
        return Ok(query);
    }
    // Capture only attributes: the body keeps its original conditional demand for argument expressions.
    let source =
        MutableSourcePlan::captured_values(Items::new(captures.finish()?), relation, control)?;
    let wrapper = project(query, width, control)?;
    let (wrapper, body) = wrapper.take_source(control)?;
    let source = MutableSourcePlan::cross_join(
        source,
        body.expect("projected query source"),
        true,
        control,
    )?;
    wrapper.replace_source(Some(source), control)
}

pub(super) fn coerce_result(
    mut query: MutableQueryPlan,
    targets: &[ColumnType],
    sources: &[Option<ColumnType>],
    control: &ProductionControl<'_>,
) -> Result<MutableQueryPlan, SQLError> {
    if targets
        .iter()
        .zip(sources)
        .all(|(target, source)| source.as_ref() == Some(target))
    {
        return Ok(query);
    }
    let needs_wrapper = match &query.root {
        RelationalPlan::QueryBlock(block) => {
            block.distinct
                || !block.distinct_on.is_empty()
                || !block.order_by.is_empty()
                || !block.group_by.is_empty()
                || !block.grouping_sets.is_empty()
        }
        _ => true,
    };
    if needs_wrapper {
        query = project(query, targets.len(), control)?;
    }
    for (index, (target, source)) in targets.iter().zip(sources).enumerate() {
        if source.as_ref() != Some(target) {
            let name = target.catalog_name_with_control(control)?;
            query =
                query.map_scalar(QueryScalarSlot::Projection(index), control, |expression| {
                    Ok(OwnedScalarExpr::cast(expression, name, true, control)?)
                })?;
        }
    }
    Ok(query)
}

fn project(
    body: MutableQueryPlan,
    width: usize,
    control: &ProductionControl<'_>,
) -> Result<MutableQueryPlan, SQLError> {
    let bound = body.relations_bound;
    let mut columns = ProductionVec::new(*control);
    columns.reserve(width)?;
    for index in 1..=width {
        columns.push_produced(control.format(format_args!("column{index}"))?)?;
    }
    let columns = columns.finish()?;
    let mut projections = ProductionVec::new(*control);
    projections.reserve(width)?;
    for name in &*columns {
        let projection = OwnedProjection {
            expression: OwnedScalarExpr::column(control.copy_text(name)?, None, control)?,
            alias: Some(control.copy_text(name)?),
        };
        projections.push_produced(control.finish(projection, control.empty_reservation())?)?;
    }
    let source = MutableSourcePlan::subquery(body, None, columns, None, false, control)?;
    MutableQueryPlan::projected(Items::new(projections.finish()?), control)?
        .set_relations_bound(bound, control)?
        .replace_source(Some(source), control)
}
