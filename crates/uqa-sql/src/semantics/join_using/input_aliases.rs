//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Null-rejection aliases borrow final output names while admitting duplicate indexes and layout owners.

use super::{production, JoinKind, ResolvedJoinUsing, RowSchema, SQLError};
use uqa_core::memory::{ProductionControl, ProductionMap, ProductionVec};

#[derive(Clone, Copy)]
enum Owner {
    Left(usize),
    Right(usize),
    Neither,
}

pub fn join_alias_input_schemas_with_control(
    kind: JoinKind,
    left: &RowSchema,
    right: &RowSchema,
    using: Option<&ResolvedJoinUsing>,
    alias: &str,
    column_aliases: &[String],
    control: &ProductionControl<'_>,
) -> Result<(RowSchema, RowSchema), SQLError> {
    let mut output = ProductionVec::new(*control);
    if let Some(using) = using {
        production::visit_column_order(kind, left.len(), right.len(), using, control, |column| {
            let pair = match column {
                production::OutputColumn::Merged(column) => (
                    column.name.as_str(),
                    match kind {
                        JoinKind::Inner | JoinKind::Left => Owner::Left(column.left),
                        JoinKind::Right => Owner::Right(column.right),
                        JoinKind::Full => Owner::Neither,
                        JoinKind::Cross => {
                            unreachable!("qualified cross join rejected by column order")
                        }
                    },
                ),
                production::OutputColumn::Left(position) => (
                    left.public_name(position)
                        .unwrap_or(&left.columns()[position]),
                    Owner::Left(position),
                ),
                production::OutputColumn::Right(position) => (
                    right
                        .public_name(position)
                        .unwrap_or(&right.columns()[position]),
                    Owner::Right(position),
                ),
            };
            output.push_copy(pair)?;
            Ok(())
        })?;
    } else {
        for (schema, is_left) in [(left, true), (right, false)] {
            for (position, column) in schema.columns().iter().enumerate() {
                output.push_copy((
                    schema.public_name(position).unwrap_or(column),
                    if is_left {
                        Owner::Left(position)
                    } else {
                        Owner::Right(position)
                    },
                ))?;
            }
        }
    }
    if column_aliases.len() > output.len() {
        return Err(SQLError::Routine {
            sqlstate: "42P10".into(),
            message: control
                .format(format_args!(
                    "join expression \"{alias}\" has {} columns available but {} columns specified",
                    output.len(),
                    column_aliases.len()
                ))?
                .into_parts()
                .0,
        });
    }
    for (position, column) in column_aliases.iter().enumerate() {
        control.check()?;
        output.as_mut_slice()[position].0 = column;
    }
    let mut counts = ProductionMap::new(*control);
    for (column, _) in output.iter() {
        counts.insert_copy(*column, counts.get(*column).copied().unwrap_or(0_usize) + 1)?;
    }
    let mut left_aliases = ProductionVec::new(*control);
    let mut right_aliases = ProductionVec::new(*control);
    for (column, owner) in output.iter() {
        control.check()?;
        if counts.get(*column) != Some(&1) {
            continue;
        }
        match owner {
            Owner::Left(position) => left_aliases.push_copy((Some(alias), *column, *position))?,
            Owner::Right(position) => right_aliases.push_copy((Some(alias), *column, *position))?,
            Owner::Neither => {}
        }
    }
    Ok((
        RowSchema::with_identity_aliases_with_control(left, left_aliases.iter().copied(), control)?,
        RowSchema::with_identity_aliases_with_control(
            right,
            right_aliases.iter().copied(),
            control,
        )?,
    ))
}

#[cfg(test)]
mod tests;
