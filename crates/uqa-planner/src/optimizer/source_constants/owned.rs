//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Constant propagation replaces scalar owners while preserving query containers and retained source authority.

use super::{map::Constants, production, RelationalPlan, RoutineTypeCatalog, SQLError, ScalarExpr};
use uqa_core::{memory::ProductionControl, Value};
use uqa_sql::{
    plan::{MutableQueryPlan, OwnedScalarExpr, QueryScalarSlot},
    semantics::volatility::VolatilityCatalog,
};

pub(in crate::optimizer) fn propagate(
    mut query: MutableQueryPlan,
    types: Option<&dyn RoutineTypeCatalog>,
    views: Option<&dyn VolatilityCatalog>,
    control: &ProductionControl<'_>,
) -> Result<MutableQueryPlan, SQLError> {
    let RelationalPlan::QueryBlock(block) = &query.root else {
        return Ok(query);
    };
    let Some(source) = &block.from else {
        return Ok(query);
    };
    let mut constants = Constants::new(control);
    if !production::observe_source(source, types, views, control, &mut |name, value| {
        constants.insert(name, value, control)
    })? {
        return Ok(query);
    }
    let qualifier = source
        .visible_qualifier()
        .map(|name| control.copy_text(name))
        .transpose()?;
    let count = block.projections.len();
    for index in 0..count {
        let RelationalPlan::QueryBlock(block) = &query.root else {
            unreachable!()
        };
        let projection = &block.projections[index];
        if projection.alias.is_none() {
            if let ScalarExpr::Column(column) | ScalarExpr::QualifiedColumn { column, .. } =
                &projection.expr
            {
                let name = control.copy_text(column)?;
                query = query.replace_projection_alias(index, Some(name), control)?;
            }
        }
    }
    map_scalars(query, control, |expression| {
        substitute(
            expression,
            qualifier.as_deref().map(String::as_str),
            &constants,
            control,
        )
    })
}

fn substitute(
    expression: OwnedScalarExpr,
    qualifier: Option<&str>,
    constants: &Constants,
    control: &ProductionControl<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    let expression = expression.map_children(control, |_, child| {
        substitute(child, qualifier, constants, control)
    })?;
    let column = match &*expression {
        ScalarExpr::Column(column) => Some(column.as_str()),
        ScalarExpr::QualifiedColumn {
            qualifier: requested,
            column,
        } if qualifier == Some(requested.as_str()) => Some(column.as_str()),
        _ => None,
    };
    if let Some(value) = column.and_then(|column| constants.get(column)) {
        let replacement = value.copy_for_rewrite(control)?;
        return super::super::scalar::constants::metadata::retain_computed_integer(
            replacement,
            None,
            control,
        );
    }
    Ok(expression)
}

pub(in crate::optimizer) fn simplify_group(
    query: MutableQueryPlan,
    types: Option<&dyn RoutineTypeCatalog>,
    control: &ProductionControl<'_>,
) -> Result<MutableQueryPlan, SQLError> {
    let RelationalPlan::QueryBlock(block) = &query.root else {
        return Ok(query);
    };
    if !production::constant_group(block, types, control)? {
        return Ok(query);
    }
    let key = OwnedScalarExpr::literal(
        control.finish(Value::Bool(true), control.empty_reservation())?,
        control,
    )?;
    query.replace_group_by(std::iter::once(key), control)
}

fn map_scalars(
    mut query: MutableQueryPlan,
    control: &ProductionControl<'_>,
    mut map: impl FnMut(OwnedScalarExpr) -> Result<OwnedScalarExpr, SQLError>,
) -> Result<MutableQueryPlan, SQLError> {
    let RelationalPlan::QueryBlock(block) = &query.root else {
        return Ok(query);
    };
    let counts = (
        block.projections.len(),
        block.group_by.len(),
        block.grouping_sets.len(),
        block.order_by.len(),
        block.distinct_on.len(),
        block.windows.len(),
    );
    for index in 0..counts.0 {
        query = query.map_scalar(QueryScalarSlot::Projection(index), control, &mut map)?;
    }
    query = query.map_scalar(QueryScalarSlot::Where, control, &mut map)?;
    for index in 0..counts.1 {
        query = query.map_scalar(QueryScalarSlot::GroupBy(index), control, &mut map)?;
    }
    for set in 0..counts.2 {
        let RelationalPlan::QueryBlock(block) = &query.root else {
            unreachable!()
        };
        for expression in 0..block.grouping_sets[set].len() {
            query = query.map_scalar(
                QueryScalarSlot::GroupingSet { set, expression },
                control,
                &mut map,
            )?;
        }
    }
    query = query.map_scalar(QueryScalarSlot::Having, control, &mut map)?;
    for index in 0..counts.3 {
        query = query.map_scalar(QueryScalarSlot::OrderBy(index), control, &mut map)?;
    }
    query = query.map_scalar(QueryScalarSlot::Limit, control, &mut map)?;
    query = query.map_scalar(QueryScalarSlot::Offset, control, &mut map)?;
    for index in 0..counts.4 {
        query = query.map_scalar(QueryScalarSlot::DistinctOn(index), control, &mut map)?;
    }
    for window in 0..counts.5 {
        let RelationalPlan::QueryBlock(block) = &query.root else {
            unreachable!()
        };
        let (partitions, orders) = (
            block.windows[window].spec.partition_by.len(),
            block.windows[window].spec.order_by.len(),
        );
        for expression in 0..partitions {
            query = query.map_scalar(
                QueryScalarSlot::WindowPartition { window, expression },
                control,
                &mut map,
            )?;
        }
        for expression in 0..orders {
            query = query.map_scalar(
                QueryScalarSlot::WindowOrder { window, expression },
                control,
                &mut map,
            )?;
        }
        query = query.map_scalar(QueryScalarSlot::WindowFrameStart(window), control, &mut map)?;
        query = query.map_scalar(QueryScalarSlot::WindowFrameEnd(window), control, &mut map)?;
    }
    Ok(query)
}
