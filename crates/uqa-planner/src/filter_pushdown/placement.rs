//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Predicate placement borrows decisions and admits only selected destination expressions.

use super::{
    context::{FilterPushdownContext, FilterPushdownScope},
    qualifier_filter_elision_safe,
    source_columns::SourceColumns,
};
use uqa_core::memory::{ProductionControl, ProductionMap, ProductionVec};
use uqa_sql::semantics::volatility::expr_contains_volatile_function_with_control;
use uqa_sql::{
    plan::{
        source_projection::QualifierFilters, OwnedScalarExpr, QueryBlockPlan, QueryPlan, SourcePlan,
    },
    semantics::{
        expr_contains_subquery, qualify_unqualified_columns_with_control,
        visit_filter_parts_with_control, visit_outer_join_nullable_qualifiers_with_control,
    },
    SQLError, ScalarExpr,
};

pub fn qualifier_filters_for_stmt_with_control(
    context: FilterPushdownContext<'_>,
    stmt: &QueryBlockPlan,
    from: &SourcePlan,
    scope: FilterPushdownScope<'_>,
    control: &ProductionControl<'_>,
) -> Result<Option<QualifierFilters>, SQLError> {
    control.check()?;
    if !stmt.deferred_outer_rewrites.is_empty() {
        return Ok(None);
    }
    let Some(filter) = stmt.r#where.as_ref() else {
        return Ok(None);
    };
    let columns = SourceColumns::new_with_control(scope, from, control)?;
    if columns.qualifiers.is_empty() {
        return Ok(None);
    }
    let mut nullable = ProductionMap::new(*control);
    visit_outer_join_nullable_qualifiers_with_control(from, control, &mut |name| {
        nullable.insert_copy(name, ())?;
        Ok(())
    })?;
    let mut filters = QualifierFilters::new();
    visit_filter_parts_with_control(filter, control, &mut |part| {
        if let Some(qualifier) =
            qualifier_for_part(context, part, &columns, &stmt.subqueries, control)?
        {
            if nullable.get(qualifier).is_none() {
                filters.push_owned(
                    qualifier,
                    qualify_unqualified_columns_with_control(part, qualifier, control)?,
                    control,
                )?;
            }
        } else if qualifier_filter_elision_safe(from) {
            derived_disjuncts(
                context,
                part,
                &columns,
                &stmt.subqueries,
                control,
                &mut |qualifier, predicate| {
                    if nullable.get(qualifier).is_none() {
                        filters.push_owned(qualifier, predicate, control)?;
                    }
                    Ok(())
                },
            )?;
        }
        Ok(())
    })?;
    Ok((!filters.is_empty()).then_some(filters))
}

pub(super) fn derived_disjuncts(
    context: FilterPushdownContext<'_>,
    part: &ScalarExpr,
    columns: &SourceColumns,
    subqueries: &[QueryPlan],
    control: &ProductionControl<'_>,
    visit: &mut dyn FnMut(&str, OwnedScalarExpr) -> Result<(), SQLError>,
) -> Result<(), SQLError> {
    let ScalarExpr::Or(disjuncts) = part else {
        return Ok(());
    };
    if disjuncts.len() < 2
        || expr_contains_subquery(part)
        || expr_contains_volatile_function_with_control(context.volatility, part, control)?
    {
        return Ok(());
    }
    for qualifier in columns.qualifiers.iter() {
        let mut projected = crate::scratch::Buffer::new(*control);
        for disjunct in disjuncts {
            let mut local = crate::scratch::Buffer::new(*control);
            visit_filter_parts_with_control(disjunct, control, &mut |conjunct| {
                if qualifier_for_part(context, conjunct, columns, subqueries, control)?
                    == Some(qualifier.as_str())
                {
                    local.push(qualify_unqualified_columns_with_control(
                        conjunct, qualifier, control,
                    )?)?;
                }
                Ok(())
            })?;
            let Some(local) = combine_owned(local, true, control)? else {
                break;
            };
            projected.push(local)?;
        }
        if projected.len() == disjuncts.len() {
            if let Some(predicate) = combine_owned(projected, false, control)? {
                visit(qualifier, predicate)?;
            }
        }
    }
    Ok(())
}

pub(super) fn combine_owned(
    mut expressions: crate::scratch::Buffer<OwnedScalarExpr>,
    conjunction: bool,
    control: &ProductionControl<'_>,
) -> Result<Option<OwnedScalarExpr>, SQLError> {
    match expressions.len() {
        0 => Ok(None),
        1 => Ok(expressions.pop()),
        _ => Ok(Some(OwnedScalarExpr::boolean(
            expressions.into_iter(),
            conjunction,
            control,
        )?)),
    }
}

pub(super) fn qualifier_for_part<'a>(
    context: FilterPushdownContext<'_>,
    part: &'a ScalarExpr,
    columns: &'a SourceColumns,
    subqueries: &[QueryPlan],
    control: &ProductionControl<'_>,
) -> Result<Option<&'a str>, SQLError> {
    control.check()?;
    let mut internal = false;
    part.try_visit(&mut |node| {
        control.check()?;
        internal |= matches!(node, ScalarExpr::InternalColumn(_));
        Ok::<_, SQLError>(!internal)
    })?;
    if internal {
        return Ok(None);
    }
    let contains_subquery = expr_contains_subquery(part);
    if contains_subquery {
        if !super::subqueries::subqueries_are_uncorrelated_and_stable_with_control(
            context, part, subqueries, control,
        )? || super::subqueries::outer_expression_contains_volatile_function_with_control(
            context, part, control,
        )? {
            return Ok(None);
        }
    } else if expr_contains_volatile_function_with_control(context.volatility, part, control)?
        && !crate::optimizer::contains_retrieval(part)
    {
        return Ok(None);
    }
    let mut qualifier = None;
    let mut multiple = false;
    let mut unqualified = false;
    part.try_visit(&mut |node| {
        control.check()?;
        if let Some(name) = node.reference_qualifier() {
            multiple |= qualifier.is_some_and(|previous| previous != name);
            qualifier = Some(name);
        }
        unqualified |= matches!(node, ScalarExpr::Column(_));
        Ok::<_, SQLError>(true)
    })?;
    if multiple {
        return Ok(None);
    }
    let reads_columns_of = |qualifier: &str| -> Result<bool, SQLError> {
        let mut valid = true;
        part.try_visit(&mut |node| {
            control.check()?;
            if let ScalarExpr::Column(column) = node {
                valid &= columns.reads_column_of(column, qualifier);
            }
            Ok::<_, SQLError>(valid)
        })?;
        Ok(valid)
    };
    if let Some(qualifier) = qualifier {
        return Ok(
            (columns.qualifiers.contains(qualifier) && reads_columns_of(qualifier)?)
                .then_some(qualifier),
        );
    }
    if !unqualified {
        return Ok(crate::optimizer::contains_retrieval(part)
            .then(|| columns.only_relation())
            .flatten());
    }
    if let Some(owner) = unique_owner(part, columns, control)? {
        if columns.qualifiers.contains(owner) {
            return Ok(Some(owner));
        }
    }
    let Some(qualifier) = columns.only_relation() else {
        return Ok(None);
    };
    Ok(reads_columns_of(qualifier)?.then_some(qualifier))
}

fn unique_owner<'a>(
    part: &ScalarExpr,
    columns: &'a SourceColumns,
    control: &ProductionControl<'_>,
) -> Result<Option<&'a str>, SQLError> {
    let mut owner = None;
    let mut valid = true;
    part.try_visit(&mut |node| {
        control.check()?;
        match node {
            ScalarExpr::Column(name) => {
                let candidate = columns
                    .owners()
                    .get(name)
                    .and_then(|owner| owner.as_ref())
                    .map(|owner| owner.as_str());
                valid &= candidate.is_some() && owner.is_none_or(|owner| candidate == Some(owner));
                owner = candidate;
            }
            ScalarExpr::Default
            | ScalarExpr::Star
            | ScalarExpr::Position(_)
            | ScalarExpr::InternalColumn(_)
            | ScalarExpr::QualifiedStar(_)
            | ScalarExpr::WindowCall { .. } => valid = false,
            _ => {}
        }
        Ok::<_, SQLError>(valid)
    })?;
    Ok(valid.then_some(owner).flatten())
}

pub fn final_filter_after_qualifier_pushdown_with_control(
    context: FilterPushdownContext<'_>,
    stmt: &QueryBlockPlan,
    from: &SourcePlan,
    filters: Option<&QualifierFilters>,
    scope: FilterPushdownScope<'_>,
    control: &ProductionControl<'_>,
) -> Result<Option<OwnedScalarExpr>, SQLError> {
    control.check()?;
    let Some(filter) = stmt.r#where.as_ref() else {
        return Ok(None);
    };
    if !qualifier_filter_elision_safe(from) {
        return Ok(Some(filter.copy_for_rewrite(control)?));
    }
    let columns = SourceColumns::new_with_control(scope, from, control)?;
    let mut guaranteed = ProductionVec::new(*control);
    collect_guaranteed(from, control, &mut guaranteed)?;
    let mut residual = crate::scratch::Buffer::new(*control);
    visit_filter_parts_with_control(filter, control, &mut |part| {
        let pushed = filters.is_some()
            && qualifier_for_part(context, part, &columns, &stmt.subqueries, control)?.is_some();
        let by_join =
            !expr_contains_volatile_function_with_control(context.volatility, part, control)?
                && guaranteed.contains(&part);
        if !pushed && !by_join {
            residual.push(part.copy_for_rewrite(control)?)?;
        }
        Ok(())
    })?;
    combine_owned(residual, true, control)
}
fn collect_guaranteed<'a>(
    source: &'a SourcePlan,
    control: &ProductionControl<'_>,
    filters: &mut ProductionVec<'_, &'a ScalarExpr>,
) -> Result<(), SQLError> {
    control.check()?;
    if let SourcePlan::Join {
        left,
        right,
        kind: uqa_sql::ast::JoinKind::Inner | uqa_sql::ast::JoinKind::Cross,
        on,
        ..
    } = source
    {
        collect_guaranteed(left, control, filters)?;
        collect_guaranteed(right, control, filters)?;
        if let Some(on) = on {
            visit_filter_parts_with_control(on, control, &mut |part| {
                filters.push_copy(part)?;
                Ok(())
            })?;
        }
    }
    Ok(())
}
