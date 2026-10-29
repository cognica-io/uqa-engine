//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! USING resolution admits borrowed name indexes and independently retained output payloads.

use super::{
    ambiguous_using_column, missing_using_column, JoinUsing, ResolvedJoinColumn, ResolvedJoinUsing,
    RowSchema, SQLError,
};
use crate::type_resolution::{
    common_type_in_with_control, equality_operand_type_with_control, CommonTypeContext,
};
use uqa_core::memory::{Produced, ProductionControl, ProductionMap, ProductionVec};

type Positions<'s, 'c> = ProductionMap<'c, &'s str, Option<usize>>;

fn positions<'s, 'c>(
    schema: &'s RowSchema,
    control: ProductionControl<'c>,
) -> Result<Positions<'s, 'c>, SQLError> {
    let mut positions = ProductionMap::new(control);
    for (position, identity) in schema.identities().iter().enumerate() {
        control.check()?;
        let name = identity.column();
        let unique = positions.get(name).is_none().then_some(position);
        positions.insert_copy(name, unique)?;
    }
    Ok(positions)
}

fn unique_position(
    positions: &Positions<'_, '_>,
    column: &str,
    side: &str,
) -> Result<usize, SQLError> {
    match positions.get(column) {
        None => Err(missing_using_column(column, side)),
        Some(Some(position)) => Ok(*position),
        Some(None) => Err(ambiguous_using_column(column, side)),
    }
}

/// Resolve the original USING/NATURAL namespace and coercions with the invoking request's admission and both cancellation scopes. The returned owner retains only its completed names, types, alias and column buffer; lookup scratch retires before return.
pub fn resolve_join_using_with_control(
    explicit: Option<&JoinUsing>,
    natural: bool,
    left: &RowSchema,
    right: &RowSchema,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ResolvedJoinUsing>>, SQLError> {
    control.check()?;
    if explicit.is_none() && !natural {
        return Ok(None);
    }
    if explicit.is_some() && natural {
        return Err(SQLError::Internal(
            "JOIN cannot be both USING-qualified and NATURAL".into(),
        ));
    }
    let left_positions = positions(left, *control)?;
    let right_positions = positions(right, *control)?;
    let mut names = ProductionVec::new(*control);
    let mut seen = ProductionMap::new(*control);
    let alias = if let Some(using) = explicit {
        names.reserve(using.columns.len())?;
        for name in &using.columns {
            if seen.insert_copy(name.as_str(), ())?.is_some() {
                return Err(SQLError::Routine {
                    sqlstate: "42701".into(),
                    message: format!(
                        "column name \"{name}\" appears more than once in USING clause"
                    ),
                });
            }
            names.push_copy(name.as_str())?;
        }
        using.alias.as_deref()
    } else {
        for identity in left.identities() {
            control.check()?;
            let name = identity.column();
            if right_positions.get(name).is_some() && seen.insert_copy(name, ())?.is_none() {
                names.push_copy(name)?;
            }
        }
        None
    };
    drop(seen);
    if let Some(alias) = alias {
        if left.has_qualifier(alias) || right.has_qualifier(alias) {
            return Err(SQLError::Routine {
                sqlstate: "42712".into(),
                message: format!("table name \"{alias}\" specified more than once"),
            });
        }
    }
    let alias = alias.map(|alias| control.copy_text(alias)).transpose()?;
    let mut columns = ProductionVec::new(*control);
    columns.reserve(names.len())?;
    for name in names.iter() {
        control.check()?;
        let left_position = unique_position(&left_positions, name, "left")?;
        let right_position = unique_position(&right_positions, name, "right")?;
        columns.push_produced(column(
            name,
            left_position,
            right_position,
            left,
            right,
            control,
        )?)?;
    }
    let columns = columns.finish()?;
    let (columns, mut memory) = columns.into_parts();
    let alias = alias.map(|alias| {
        let (alias, lease) = alias.into_parts();
        memory = control.combine(memory.take(), lease);
        alias
    });
    Ok(Some(
        control.finish(ResolvedJoinUsing { columns, alias }, memory)?,
    ))
}

fn column(
    name: &str,
    left_position: usize,
    right_position: usize,
    left: &RowSchema,
    right: &RowSchema,
    control: &ProductionControl<'_>,
) -> Result<Produced<ResolvedJoinColumn>, SQLError> {
    let (comparison_type, output_type) = match (
        left.column_type(left_position),
        right.column_type(right_position),
    ) {
        (Some(left_type), Some(right_type)) => (
            Some(equality_operand_type_with_control(
                left_type, right_type, control,
            )?),
            Some(common_type_in_with_control(
                CommonTypeContext::JoinUsing,
                left_type,
                right_type,
                control,
            )?),
        ),
        _ => (None, None),
    };
    let name = control.copy_text(name)?;
    let (name, mut memory) = name.into_parts();
    let comparison_type = comparison_type.map(|ty| {
        let (ty, lease) = ty.into_parts();
        memory = control.combine(memory.take(), lease);
        ty
    });
    let output_type = output_type.map(|ty| {
        let (ty, lease) = ty.into_parts();
        memory = control.combine(memory.take(), lease);
        ty
    });
    Ok(control.finish(
        ResolvedJoinColumn {
            name,
            left: left_position,
            right: right_position,
            comparison_type,
            output_type,
        },
        memory,
    )?)
}

#[cfg(test)]
mod tests;
