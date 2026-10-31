//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Trailing routine identities preserve physical slots and carry independent controlled lookup metadata.

use crate::{
    schema::{RowSchema, SchemaMap, NULL_SLOT},
    SQLError,
};
use uqa_core::memory::ProductionControl;

impl RowSchema {
    pub fn with_trailing_internal_aliases_with_control(
        input: &Self,
        trailing: &Self,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        let offset = input
            .physical_width()
            .checked_sub(trailing.physical_width())
            .expect("trailing internal-alias scope exceeds input row width");
        for (column, slot) in &trailing.index.executor_attributes {
            control.check()?;
            if let Some(previous) = input.index.executor_attributes.get(column) {
                assert_eq!(
                    *previous,
                    shifted(*slot, offset),
                    "structural identity moved between row scopes"
                );
            }
        }
        for (column, ty) in &trailing.index.cold.executor_attribute_types {
            control.check()?;
            if let Some(previous) = input.index.cold.executor_attribute_types.get(column) {
                assert_eq!(
                    previous, ty,
                    "structural identity type changed between row scopes"
                );
            }
        }
        let mut metadata = super::copy::metadata(input, control)?;
        metadata.internal = SchemaMap::from_produced(
            input
                .index
                .executor_attributes
                .iter()
                .map(|(column, slot)| {
                    Ok((
                        control.finish(*column, control.empty_reservation())?,
                        control.finish(*slot, control.empty_reservation())?,
                    ))
                })
                .chain(
                    trailing
                        .index
                        .executor_attributes
                        .iter()
                        .filter(|(column, _)| {
                            !input.index.executor_attributes.contains_key(*column)
                        })
                        .map(|(column, slot)| {
                            Ok((
                                control.finish(*column, control.empty_reservation())?,
                                control
                                    .finish(shifted(*slot, offset), control.empty_reservation())?,
                            ))
                        }),
                ),
            control,
        )?;
        metadata.internal_types = SchemaMap::from_produced(
            input
                .index
                .cold
                .executor_attribute_types
                .iter()
                .map(|(column, ty)| {
                    Ok((
                        control.finish(*column, control.empty_reservation())?,
                        super::optional_type(ty.as_ref(), control)?,
                    ))
                })
                .chain(
                    trailing
                        .index
                        .cold
                        .executor_attribute_types
                        .iter()
                        .filter(|(column, _)| {
                            !input
                                .index
                                .cold
                                .executor_attribute_types
                                .contains_key(*column)
                        })
                        .map(|(column, ty)| {
                            Ok((
                                control.finish(*column, control.empty_reservation())?,
                                super::optional_type(ty.as_ref(), control)?,
                            ))
                        }),
                ),
            control,
        )?;
        for (column, ty) in &trailing.index.cold.executor_attribute_types {
            metadata.collations.internal_with_control(
                *column,
                trailing.internal_collation(*column),
                ty.as_ref(),
                control,
            )?;
        }
        super::copy::with_metadata(input, metadata, control)
    }
}

fn shifted(slot: usize, offset: usize) -> usize {
    if slot == NULL_SLOT {
        NULL_SLOT
    } else {
        offset + slot
    }
}
