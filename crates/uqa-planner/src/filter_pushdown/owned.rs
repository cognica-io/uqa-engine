//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Output substitution copies admitted constructors and keeps every replaced child independently owned.

use uqa_core::memory::{ProductionControl, ProductionMap, ProductionVec};
use uqa_sql::{
    plan::{OwnedScalarExpr, ProjectionPlan, QueryBlockPlan},
    routines::RoutineResolution,
    semantics::volatility::VolatilityCatalog,
    SQLError, ScalarExpr,
};

pub(crate) fn rewrite_output_filter(
    expression: &ScalarExpr,
    qualifier: &str,
    columns: &[String],
    projections: &[ProjectionPlan],
    used: &mut dyn FnMut(usize) -> Result<(), SQLError>,
    retain_query_slots: bool,
    control: &ProductionControl<'_>,
) -> Result<Option<OwnedScalarExpr>, SQLError> {
    let mut valid = true;
    expression.try_visit(&mut |node| {
        control.check()?;
        if !valid {
            return Ok(false);
        }
        match node {
            ScalarExpr::Column(column) => {
                if let Some(position) = columns.iter().position(|name| name == column) {
                    used(position)?;
                } else {
                    valid = false;
                }
            }
            ScalarExpr::QualifiedColumn {
                qualifier: owner,
                column,
            } if owner == qualifier => {
                if let Some(position) = columns.iter().position(|name| name == column) {
                    used(position)?;
                } else {
                    valid = false;
                }
            }
            ScalarExpr::ScalarSubquery(_)
            | ScalarExpr::Exists { .. }
            | ScalarExpr::InSubquery { .. }
                if retain_query_slots => {}
            ScalarExpr::Default
            | ScalarExpr::Position(_)
            | ScalarExpr::InternalColumn(_)
            | ScalarExpr::QualifiedColumn { .. }
            | ScalarExpr::Star
            | ScalarExpr::QualifiedStar(_)
            | ScalarExpr::WindowCall { .. }
            | ScalarExpr::ScalarSubquery(_)
            | ScalarExpr::Exists { .. }
            | ScalarExpr::InSubquery { .. } => valid = false,
            ScalarExpr::Literal(_)
            | ScalarExpr::TypedLiteral { .. }
            | ScalarExpr::RetainedLiteral(_)
            | ScalarExpr::Param(_)
            | ScalarExpr::Array(_)
            | ScalarExpr::Row(_)
            | ScalarExpr::CompositeRow { .. }
            | ScalarExpr::Func { .. }
            | ScalarExpr::Binary { .. }
            | ScalarExpr::Not(_)
            | ScalarExpr::UnaryMinus(_)
            | ScalarExpr::And(_)
            | ScalarExpr::Or(_)
            | ScalarExpr::IsNull { .. }
            | ScalarExpr::Between { .. }
            | ScalarExpr::InList { .. }
            | ScalarExpr::Case { .. }
            | ScalarExpr::Collated { .. }
            | ScalarExpr::Cast { .. } => {}
        }
        Ok::<_, SQLError>(valid)
    })?;
    if !valid {
        return Ok(None);
    }
    Ok(Some(replace(
        expression.copy_for_rewrite(control)?,
        columns,
        projections,
        control,
    )?))
}

fn replace(
    expression: OwnedScalarExpr,
    columns: &[String],
    projections: &[ProjectionPlan],
    control: &ProductionControl<'_>,
) -> Result<OwnedScalarExpr, SQLError> {
    match &*expression {
        ScalarExpr::Column(column) | ScalarExpr::QualifiedColumn { column, .. } => {
            let index = columns
                .iter()
                .position(|name| name == column)
                .expect("validated output reference");
            let output = projections[index].expr.copy_for_rewrite(control)?;
            drop(expression);
            Ok(output)
        }
        _ => expression.map_children(control, |_, child| {
            replace(child, columns, projections, control)
        }),
    }
}

#[derive(Clone, Copy)]
pub(crate) struct PredicateServices<'a> {
    pub routines: &'a dyn RoutineResolution,
    pub volatility: &'a dyn VolatilityCatalog,
}

pub(crate) fn derived_output_predicate(
    services: PredicateServices<'_>,
    block: &QueryBlockPlan,
    qualifier: &str,
    filter: &ScalarExpr,
    aliases: Option<&[String]>,
    retain_query_slots: bool,
    control: &ProductionControl<'_>,
) -> Result<Option<OwnedScalarExpr>, SQLError> {
    control.check()?;
    if block.limit.is_some()
        || block.offset.is_some()
        || !block.grouping_sets.is_empty()
        || aliases.is_some_and(|aliases| aliases.len() > block.projections.len())
    {
        return Ok(None);
    }
    let mut names = ProductionVec::new(*control);
    for (index, projection) in block.projections.iter().enumerate() {
        let name = match aliases.and_then(|names| names.get(index)) {
            Some(name) => control.copy_text(name)?,
            None => uqa_sql::semantics::with_projection_label(projection, control, |name| {
                Ok(control.copy_text(name)?)
            })?,
        };
        names.push_produced(name)?;
    }
    let mut used = ProductionMap::new(*control);
    let rewritten = rewrite_output_filter(
        filter,
        qualifier,
        &names,
        &block.projections,
        &mut |index| {
            used.insert_copy(index, ())?;
            Ok(())
        },
        retain_query_slots,
        control,
    )?;
    let Some(rewritten) = rewritten else {
        return Ok(None);
    };
    if used.is_empty() {
        return Ok(None);
    }
    for (index, ()) in &used {
        let expression = &block.projections[*index].expr;
        if matches!(expression, ScalarExpr::Star)
            || expression.contains_window()
            || uqa_sql::semantics::expr_contains_subquery(expression)
            || uqa_sql::semantics::volatility::expr_contains_volatile_function_with_control(services.volatility, expression, control)?
            || uqa_sql::semantics::sets::static_setness::expression_may_return_set_statically_with_control(services.routines, expression, control)?
            || !super::output_boundary::preserves_groups_with_control(block, expression, control)?
        {
            return Ok(None);
        }
    }
    Ok(Some(rewritten))
}
