//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Qualified JOIN output selection admits each copied identity and coercion before construction.

use super::{
    production::{visit_column_order, OutputColumn},
    ColumnIdentity, JoinKind, JoinOutputSource, JoinUsingLayout, ProductionControl,
    ResolvedJoinColumn, ResolvedJoinUsing, RowSchema, SQLError,
};
use uqa_core::memory::{Produced, ProductionVec};

pub fn join_using_output_schema_with_control(
    kind: JoinKind,
    left: &RowSchema,
    right: &RowSchema,
    using: &ResolvedJoinUsing,
    control: &ProductionControl<'_>,
) -> Result<RowSchema, SQLError> {
    let input = RowSchema::join_with_control(left, right, std::iter::empty::<String>(), control)?;
    let layout = join_using_layout_with_control(kind, left, right, using, control)?;
    crate::schema::join_output::compile_layout_with_control(&input, &layout.0, &layout.1, control)
        .map(|(schema, _)| schema)
}

pub fn join_using_layout_with_control(
    kind: JoinKind,
    left: &RowSchema,
    right: &RowSchema,
    using: &ResolvedJoinUsing,
    control: &ProductionControl<'_>,
) -> Result<Produced<JoinUsingLayout>, SQLError> {
    let mut columns = ProductionVec::new(*control);
    visit_column_order(kind, left.len(), right.len(), using, control, |column| {
        let (name, identity, source) = match column {
            OutputColumn::Merged(column) => (
                column.name.as_str(),
                ColumnIdentity::from_parts_with_control(None, &column.name, control)?,
                merged_source(kind, column, left, right, control)?,
            ),
            OutputColumn::Left(position) => (
                left.public_name(position)
                    .unwrap_or(&left.columns()[position]),
                left.identities()[position].clone_with_control(control)?,
                control.finish(
                    JoinOutputSource::Input(position),
                    control.empty_reservation(),
                )?,
            ),
            OutputColumn::Right(position) => (
                right
                    .public_name(position)
                    .unwrap_or(&right.columns()[position]),
                right.identities()[position].clone_with_control(control)?,
                control.finish(
                    JoinOutputSource::Input(left.len() + position),
                    control.empty_reservation(),
                )?,
            ),
        };
        let (name, names) = control.copy_text(name)?.into_parts();
        let (identity, identities) = identity.into_parts();
        let (source, sources) = source.into_parts();
        columns.push_produced(control.finish(
            (name, identity, source),
            control.combine(names, control.combine(identities, sources)),
        )?)?;
        Ok(())
    })?;
    let mut aliases = ProductionVec::new(*control);
    for (input, offset) in [(left, 0), (right, left.len())] {
        for (position, identity) in input.identities().iter().enumerate() {
            control.check()?;
            if identity.qualifier().is_some() {
                let (identity, memory) = identity.clone_with_control(control)?.into_parts();
                aliases.push_produced(control.finish(
                    (identity, JoinOutputSource::Input(offset + position)),
                    memory,
                )?)?;
            }
        }
    }
    if let Some(alias) = &using.alias {
        for (name, _, source) in columns.iter().take(using.columns.len()) {
            let (identity, names) =
                ColumnIdentity::from_parts_with_control(Some(alias), name, control)?.into_parts();
            let (source, sources) = source.clone_with_control(control)?.into_parts();
            aliases.push_produced(
                control.finish((identity, source), control.combine(names, sources))?,
            )?;
        }
    }
    let (columns, column_memory) = columns.finish()?.into_parts();
    let (aliases, alias_memory) = aliases.finish()?.into_parts();
    Ok(control.finish(
        (columns, aliases),
        control.combine(column_memory, alias_memory),
    )?)
}

fn merged_source(
    kind: JoinKind,
    column: &ResolvedJoinColumn,
    left: &RowSchema,
    right: &RowSchema,
    control: &ProductionControl<'_>,
) -> Result<Produced<JoinOutputSource>, SQLError> {
    if kind == JoinKind::Full {
        return match &column.output_type {
            Some(ty) => {
                let (ty, memory) = ty.clone_with_control(control)?.into_parts();
                Ok(control.finish(
                    JoinOutputSource::Coalesce {
                        left: column.left,
                        right: left.len() + column.right,
                        ty,
                    },
                    memory,
                )?)
            }
            None => Ok(control.finish(
                JoinOutputSource::Input(column.left),
                control.empty_reservation(),
            )?),
        };
    }
    let (position, source) = match kind {
        JoinKind::Inner | JoinKind::Left => (column.left, left.column_type(column.left)),
        JoinKind::Right => (left.len() + column.right, right.column_type(column.right)),
        JoinKind::Full | JoinKind::Cross => {
            unreachable!("qualified cross join rejected by column selection")
        }
    };
    if let (Some(source), Some(target)) = (source, column.output_type.as_ref()) {
        if source != target {
            let (ty, memory) = target.clone_with_control(control)?.into_parts();
            return Ok(control.finish(
                JoinOutputSource::Cast {
                    input: position,
                    ty,
                },
                memory,
            )?);
        }
    }
    Ok(control.finish(
        JoinOutputSource::Input(position),
        control.empty_reservation(),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ast::JoinUsing, semantics::resolve_join_using, ColumnType};
    use uqa_core::{memory::MemoryBudget, CancellationToken};

    #[test]
    fn using_output_payloads_retain_exact_allocated_storage_and_retire_scratch() {
        let left = RowSchema::with_qualified_types(
            "l",
            vec!["id".into(), "payload".into()],
            vec![Some(ColumnType::Integer), Some(ColumnType::Text)],
        );
        let right = RowSchema::with_qualified_types(
            "r",
            vec!["id".into(), "payload".into()],
            vec![Some(ColumnType::BigInteger), Some(ColumnType::Text)],
        );
        let clause = JoinUsing {
            columns: vec!["id".into()],
            alias: Some("merged".into()),
        };
        let using = resolve_join_using(Some(&clause), false, &left, &right)
            .unwrap()
            .unwrap();
        for kind in [
            JoinKind::Inner,
            JoinKind::Left,
            JoinKind::Right,
            JoinKind::Full,
        ] {
            let memory = MemoryBudget::new(1 << 20);
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let control = ProductionControl::new(&memory, &original, &invoking);
            let mut result = None;
            let measured = allocation_counter::measure(|| {
                result = Some(
                    join_using_layout_with_control(kind, &left, &right, &using, &control).unwrap(),
                );
            });
            let result = result.unwrap();
            assert_eq!(result.0.len(), 3);
            assert_eq!(result.1.len(), 5);
            assert_eq!(measured.bytes_current, result.reserved_bytes() as i64);
            assert_eq!(memory.used(), result.reserved_bytes());
            drop(result);
            assert_eq!(memory.used(), 0);
            for token in [&original, &invoking] {
                token.cancel();
                assert_eq!(
                    join_using_layout_with_control(kind, &left, &right, &using, &control)
                        .unwrap_err()
                        .sqlstate(),
                    Some("57014")
                );
                assert_eq!(memory.used(), 0);
                token.reset();
            }
        }
    }
}
