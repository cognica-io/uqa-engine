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
use crate::scratch::Buffer;
use uqa_core::memory::{OwnedMap, ProductionControl, ProductionMap, ProductionVec};
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
    control: &ProductionControl<'_>,
    visit: &mut dyn FnMut(&str, OwnedScalarExpr) -> Result<(), SQLError>,
) -> Result<(), SQLError> {
    struct Projection<'a> {
        branches: usize,
        terms: Buffer<(usize, &'a ScalarExpr)>,
    }

    let ScalarExpr::Or(disjuncts) = part else {
        return Ok(());
    };
    if disjuncts.len() < 2
        || expr_contains_subquery(part)
        || expr_contains_volatile_function_with_control(context.volatility, part, control)?
    {
        return Ok(());
    }
    let mut projections = OwnedMap::<&str, Projection<'_>>::new();
    for (branch, disjunct) in disjuncts.iter().enumerate() {
        let mut matching = 0;
        visit_filter_parts_with_control(disjunct, control, &mut |conjunct| {
            let Some(references) = PredicateColumns::read(conjunct, columns, control)? else {
                return Ok(());
            };
            let Some(owner) = references.qualifier(conjunct, columns) else {
                return Ok(());
            };
            if branch == 0 && !projections.contains_key(owner) {
                let projection = Projection {
                    branches: 0,
                    terms: Buffer::new(*control),
                };
                if let Some(budget) = control.budget() {
                    projections.insert_budgeted(owner, projection, budget)?;
                } else {
                    projections.insert(owner, projection);
                }
            }
            if let Some(projection) = projections.get_mut(owner) {
                if projection.branches >= branch {
                    if projection.branches == branch {
                        projection.branches += 1;
                        matching += 1;
                    }
                    projection.terms.push((branch, conjunct))?;
                }
            }
            Ok(())
        })?;
        if matching == 0 {
            return Ok(());
        }
    }

    for (qualifier, projection) in projections {
        control.check()?;
        if projection.branches != disjuncts.len() {
            continue;
        }
        let mut alternatives = Buffer::new(*control);
        for terms in projection.terms.chunk_by(|left, right| left.0 == right.0) {
            let mut predicates = Buffer::new(*control);
            for &(_, predicate) in terms {
                predicates.push(qualify_unqualified_columns_with_control(
                    predicate, qualifier, control,
                )?)?;
            }
            if let Some(conjunction) = combine_owned(predicates, true, control)? {
                alternatives.push(conjunction)?;
            }
        }
        if let Some(predicate) = combine_owned(alternatives, false, control)? {
            visit(qualifier, predicate)?;
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
    let Some(references) = PredicateColumns::read(part, columns, control)? else {
        return Ok(None);
    };
    // Internal columns exclude placement before any catalog lookup. Subqueries
    // validate their scopes before the enclosing expression's functions.
    if references.has_subquery {
        if !super::subqueries::subqueries_are_uncorrelated_and_stable_with_control(
            context, part, subqueries, control,
        )? || (references.has_calls
            && super::subqueries::outer_expression_contains_volatile_function_with_control(
                context, part, control,
            )?)
        {
            return Ok(None);
        }
    } else if references.has_calls
        && expr_contains_volatile_function_with_control(context.volatility, part, control)?
        && !crate::optimizer::contains_retrieval(part)
    {
        return Ok(None);
    }
    Ok(references.qualifier(part, columns))
}

#[derive(Clone, Copy, Default)]
enum ColumnSource<'a> {
    #[default]
    Unseen,
    One(&'a str),
    Unresolved,
}

impl<'a> ColumnSource<'a> {
    fn include(&mut self, next: Option<&'a str>) {
        *self = match (*self, next) {
            (Self::Unseen, Some(name)) => Self::One(name),
            (Self::One(old), Some(name)) if old == name => Self::One(old),
            _ => Self::Unresolved,
        };
    }
}

struct PredicateColumns<'a> {
    explicit: ColumnSource<'a>,
    unqualified: ColumnSource<'a>,
    infer_owner: bool,
    has_calls: bool,
    has_subquery: bool,
}

impl<'a> PredicateColumns<'a> {
    fn read(
        part: &'a ScalarExpr,
        columns: &'a SourceColumns,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Self>, SQLError> {
        let mut result = Self {
            explicit: ColumnSource::Unseen,
            unqualified: ColumnSource::Unseen,
            infer_owner: true,
            has_calls: false,
            has_subquery: false,
        };
        let mut internal = false;
        part.try_visit(&mut |node| {
            control.check()?;
            if internal {
                return Ok::<_, SQLError>(false);
            }
            if let Some(name) = node.reference_qualifier() {
                result.explicit.include(Some(name));
            }
            match node {
                ScalarExpr::Column(name) => {
                    result.unqualified.include(columns.readable_owner(name));
                }
                ScalarExpr::InternalColumn(_) => internal = true,
                ScalarExpr::Func { .. } => result.has_calls = true,
                ScalarExpr::WindowCall { .. } => {
                    result.has_calls = true;
                    result.infer_owner = false;
                }
                ScalarExpr::Default
                | ScalarExpr::Star
                | ScalarExpr::Position(_)
                | ScalarExpr::QualifiedStar(_) => result.infer_owner = false,
                ScalarExpr::ScalarSubquery(_)
                | ScalarExpr::Exists { .. }
                | ScalarExpr::InSubquery { .. } => result.has_subquery = true,
                _ => {}
            }
            Ok(!internal)
        })?;
        Ok((!internal).then_some(result))
    }

    fn qualifier(&self, part: &ScalarExpr, columns: &'a SourceColumns) -> Option<&'a str> {
        match (self.explicit, self.unqualified) {
            (ColumnSource::One(name), ColumnSource::Unseen)
                if columns.qualifiers.contains(name) =>
            {
                Some(name)
            }
            (ColumnSource::One(name), ColumnSource::One(owner))
                if name == owner && columns.qualifiers.contains(name) =>
            {
                Some(name)
            }
            (ColumnSource::Unseen, ColumnSource::One(owner))
                if columns.qualifiers.contains(owner)
                    && (self.infer_owner || columns.only_relation() == Some(owner)) =>
            {
                Some(owner)
            }
            (ColumnSource::Unseen, ColumnSource::Unseen)
                if self.has_calls && crate::optimizer::contains_retrieval(part) =>
            {
                columns.only_relation()
            }
            _ => None,
        }
    }
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
