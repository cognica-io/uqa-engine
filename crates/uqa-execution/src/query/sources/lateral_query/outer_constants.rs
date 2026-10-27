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
    plan::{source_projection::QualifierFilters, QueryBlockPlan},
    ScalarExpr,
};

/// Conjuncts placed in the block's sources and the remaining filter over the overlaid rows.
pub(super) struct Placement {
    pub(super) filters: Option<QualifierFilters>,
    pub(super) residual: Option<ScalarExpr>,
}

/// Place the block's WHERE conjuncts with this invocation's outer constants bound. A block whose filter reads no bindable outer column keeps its complete filter above the overlay.
pub(super) fn placement<S: Clone + Send + Sync + 'static>(
    context: &SourceContext<'_, S>,
    statement: &QueryBlockPlan,
    own_schema: &RowSchema,
    outer_row: &OwnedPhysicalRow,
    scope: &CteScope<S>,
) -> Result<Placement, SQLError> {
    let unplaced = || Placement {
        filters: None,
        residual: statement.r#where.clone(),
    };
    let Some(from) = statement.from.as_ref() else {
        return Ok(unplaced());
    };
    let Some(bound) = statement
        .r#where
        .as_ref()
        .and_then(|filter| bind_outer_constants(filter, own_schema, outer_row))
    else {
        return Ok(unplaced());
    };
    let mut bound_statement = statement.clone();
    bound_statement.r#where = Some(bound);
    let filters = context
        .planning
        .qualifier_filters(&bound_statement, from, scope)?;
    let residual =
        context
            .planning
            .residual_filter(&bound_statement, from, filters.as_ref(), scope)?;
    Ok(Placement { filters, residual })
}

/// Replace the outer references of `filter` that the block's own sources do not shadow. `None` means no reference was replaced.
pub(super) fn bind_outer_constants(
    filter: &ScalarExpr,
    own_schema: &RowSchema,
    outer_row: &OwnedPhysicalRow,
) -> Option<ScalarExpr> {
    let mut bound = filter.clone();
    let mut replaced = false;
    bound.visit_mut(&mut |expression| {
        if let Some(constant) = outer_constant(expression, own_schema, outer_row) {
            *expression = constant;
            replaced = true;
        }
    });
    replaced.then_some(bound)
}

fn outer_constant(
    expression: &ScalarExpr,
    own_schema: &RowSchema,
    outer_row: &OwnedPhysicalRow,
) -> Option<ScalarExpr> {
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
    Some(ScalarExpr::TypedLiteral {
        collation_oid: None,
        composite_source: None,
        value: outer_row.view().value_at(position)?.clone(),
        ty: ty.catalog_name(),
        bound_type: Some(ty.clone()),
        parameter_index: None,
    })
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
