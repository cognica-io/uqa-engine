//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected source predicates keep ordered scalar owners through physical preparation.

use crate::{
    plan::{source_projection::QualifierFilters, OwnedScalarExpr},
    SQLError, ScalarExpr,
};
use uqa_core::memory::{ProductionControl, ProductionVec};

pub fn combine_filter_refs_with_control<'a>(
    filters: impl IntoIterator<Item = &'a ScalarExpr>,
    control: &ProductionControl<'_>,
) -> Result<Option<OwnedScalarExpr>, SQLError> {
    control.check()?;
    let mut filters = filters.into_iter();
    let Some(first) = filters.next() else {
        return Ok(None);
    };
    let Some(second) = filters.next() else {
        return Ok(Some(first.copy_for_rewrite(control)?));
    };
    let mut children = ProductionVec::new(*control);
    for expression in [first, second].into_iter().chain(filters) {
        children.reserve(1)?;
        let child = expression.copy_for_rewrite(control)?;
        children.push_produced(control.finish(child, control.empty_reservation())?)?;
    }
    let (children, _memory) = children.finish()?.into_parts();
    Ok(Some(OwnedScalarExpr::boolean(
        children.into_iter(),
        true,
        control,
    )?))
}

pub fn qualifier_filter_with_control(
    filters: Option<&QualifierFilters>,
    qualifier: &str,
    control: &ProductionControl<'_>,
) -> Result<Option<OwnedScalarExpr>, SQLError> {
    combine_filter_refs_with_control(
        filters
            .and_then(|filters| filters.get(qualifier))
            .into_iter()
            .flatten(),
        control,
    )
}

#[cfg(test)]
mod tests;
