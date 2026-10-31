//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Outer columns are constants during one invocation of a correlated block. Binding them as typed literals, as custom plans bind parameter values, lets ordinary conjunct placement put the block's filters in its sources instead of filtering every source row after the outer row is overlaid on it.

use super::{CteScope, SQLError, SourceContext};
use crate::{OwnedPhysicalRow, RowSchema};
use uqa_sql::{
    ast::ColumnType,
    plan::OwnedScalarExpr,
    plan::{source_projection::QualifierFilters, QueryBlockPlan},
    ScalarExpr,
};

/// Conjuncts placed in the block's sources and the remaining filter over the overlaid rows.
pub(super) struct Placement {
    pub(super) filters: Option<QualifierFilters>,
    pub(super) residual: Option<OwnedScalarExpr>,
}

/// Place the block's WHERE conjuncts with this invocation's outer constants bound. A block whose filter reads no bindable outer column keeps its complete filter above the overlay.
pub(super) fn placement<S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'_, S>,
    statement: &QueryBlockPlan,
    own_schema: &RowSchema,
    outer_row: &OwnedPhysicalRow,
    scope: &CteScope<S>,
) -> Result<Placement, SQLError> {
    let resources = context.ctes.production_resources()?;
    let control = resources.control();
    let unplaced = || -> Result<Placement, SQLError> {
        Ok(Placement {
            filters: None,
            residual: statement
                .r#where
                .as_ref()
                .map(|filter| filter.copy_for_rewrite(&control))
                .transpose()?,
        })
    };
    let Some(from) = statement.from.as_ref() else {
        return unplaced();
    };
    let Some(filter) = statement.r#where.as_ref() else {
        return unplaced();
    };
    let Some(bound) = bind_outer_constants_with_control(filter, own_schema, outer_row, &control)?
    else {
        return unplaced();
    };
    let bound_owner = statement
        .copy_for_rewrite(&control)?
        .replace_where(Some(bound), &control)?;
    let uqa_sql::plan::RelationalPlan::QueryBlock(bound_statement) = &bound_owner.root else {
        unreachable!("copied query block")
    };

    let filters = context
        .planning
        .qualifier_filters(bound_statement, from, scope)?;
    let residual =
        context
            .planning
            .residual_filter(bound_statement, from, filters.as_ref(), scope)?;
    Ok(Placement { filters, residual })
}

/// Replace the outer references of `filter` that the block's own sources do not shadow. `None` means no reference was replaced.
#[cfg(test)]
pub(super) fn bind_outer_constants(
    filter: &ScalarExpr,
    own_schema: &RowSchema,
    outer_row: &OwnedPhysicalRow,
) -> Option<ScalarExpr> {
    let control = uqa_core::memory::ProductionControl::uncontrolled();
    bind_outer_constants_with_control(filter, own_schema, outer_row, &control)
        .unwrap()
        .map(|filter| {
            filter
                .finish(&control)
                .unwrap()
                .into_uncontrolled()
                .expect("ordinary outer constant")
        })
}

fn bind_outer_constants_with_control(
    filter: &ScalarExpr,
    own_schema: &RowSchema,
    outer_row: &OwnedPhysicalRow,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<Option<OwnedScalarExpr>, SQLError> {
    let mut replaced = false;
    let output = bind(
        filter.copy_for_rewrite(control)?,
        own_schema,
        outer_row,
        control,
        &mut replaced,
    )?;
    Ok(replaced.then_some(output))
}
fn bind(
    expression: OwnedScalarExpr,
    own_schema: &RowSchema,
    outer_row: &OwnedPhysicalRow,
    control: &uqa_core::memory::ProductionControl<'_>,
    replaced: &mut bool,
) -> Result<OwnedScalarExpr, SQLError> {
    if let Some((position, ty)) = outer_constant(&expression, own_schema, outer_row) {
        if let Some(value) = outer_row.view().value_at(position) {
            let output = OwnedScalarExpr::typed_literal(
                control.copy_value(value)?,
                ty.catalog_name_with_control(control)?,
                Some(ty.clone_with_control(control)?),
                None,
                None,
                control,
            )?;
            *replaced = true;
            return Ok(output);
        }
    }
    expression.map_children(control, |_, child| {
        bind(child, own_schema, outer_row, control, replaced)
    })
}

fn outer_constant<'a>(
    expression: &ScalarExpr,
    own_schema: &RowSchema,
    outer_row: &'a OwnedPhysicalRow,
) -> Option<(usize, &'a ColumnType)> {
    let outer = &outer_row.schema;
    let (position, ty) = match expression {
        // A name of the block's own sources shadows the outer scope, and an ambiguous outer name is left to the canonical resolution and its diagnostic.
        ScalarExpr::Column(name)
            if !own_schema.columns_are_open(None)
                && !own_schema.has_unqualified_column(name)
                && !own_schema.column_is_ambiguous(name)
                && !own_schema.has_qualifier(name)
                && !outer.column_is_ambiguous(name) =>
        {
            (outer.unqualified_position(name)?, outer.type_of(name)?)
        }
        ScalarExpr::QualifiedColumn { qualifier, column }
            if !own_schema.has_qualifier(qualifier) =>
        {
            (
                outer.qualified_position(qualifier, column)?,
                outer.qualified_type(qualifier, column)?,
            )
        }
        _ => return None,
    };
    if !comparable_as_literal(ty) {
        return None;
    }
    Some((position, ty))
}

/// Builtin types without a collation or catalog descriptor, whose literal compares exactly as the column value it replaces.
const fn comparable_as_literal(ty: &ColumnType) -> bool {
    matches!(
        ty,
        ColumnType::SmallInteger
            | ColumnType::Integer
            | ColumnType::BigInteger
            | ColumnType::Oid
            | ColumnType::Boolean
            | ColumnType::Real
            | ColumnType::DoublePrecision
            | ColumnType::Numeric { .. }
            | ColumnType::Date
            | ColumnType::Time
            | ColumnType::TimePrecision(_)
            | ColumnType::Timestamp
            | ColumnType::TimestampPrecision(_)
            | ColumnType::TimestampTz
            | ColumnType::TimestampTzPrecision(_)
            | ColumnType::Interval
            | ColumnType::Uuid
    )
}

#[cfg(test)]
mod tests;
