//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Captured structural attributes retain selected physical values and declared lookup types independently of SQL names.

use crate::{
    ast::InternalColumnRef,
    schema::{ColumnIdentity, RowSchema, SchemaMap, NULL_SLOT},
    ColumnType, SQLError,
};
use uqa_core::memory::{ProductionControl, ProductionMap};

type Alias<'a> = (usize, Option<&'a ColumnType>, Option<u32>);

impl RowSchema {
    pub fn with_physical_internal_aliases_with_control<'a>(
        input: &'a Self,
        aliases: impl IntoIterator<Item = (InternalColumnRef, usize, Option<&'a ColumnType>)>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let mut selected = ProductionMap::new(*control);
        for (column, slot, ty) in aliases {
            assert!(
                slot < input.physical_width(),
                "physical internal alias is outside row width"
            );
            selected.insert_copy(column, (slot, ty, input.inherited_physical_collation(slot)))?;
        }
        publish(input, &selected, control, |_| Ok(()))
    }

    pub fn with_named_internal_aliases_with_control(
        input: &Self,
        aliases: &[(InternalColumnRef, &ColumnIdentity)],
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let mut selected = ProductionMap::new(*control);
        for (column, identity) in aliases {
            control.check()?;
            if input.has_internal_column(*column) || identity.qualifier().is_none() {
                continue;
            }
            let exists = input.index.qualified.contains_key(*identity)
                || input.index.aliases.contains_key(*identity)
                || input.index.cold.binding_only.contains_key(*identity);
            if !exists {
                continue;
            }
            let (slot, ty) = if input.index.ambiguous_qualified.contains(*identity) {
                (NULL_SLOT, None)
            } else {
                (
                    input
                        .index
                        .qualified
                        .get(*identity)
                        .and_then(|logical| input.slot(*logical))
                        .or_else(|| input.index.aliases.get(*identity).copied())
                        .unwrap_or(NULL_SLOT),
                    input
                        .index
                        .qualified
                        .get(*identity)
                        .and_then(|logical| input.column_type(*logical))
                        .or_else(|| {
                            input
                                .index
                                .cold
                                .aliases
                                .get(*identity)
                                .and_then(Option::as_ref)
                        })
                        .or_else(|| {
                            input
                                .index
                                .cold
                                .binding_only
                                .get(*identity)
                                .and_then(Option::as_ref)
                        }),
                )
            };
            selected.insert_copy(
                *column,
                (slot, ty, input.qualified_identity_collation(identity)),
            )?;
        }
        publish(input, &selected, control, |metadata| {
            for (column, identity) in aliases {
                if !input.has_internal_column(*column) && identity.qualifier().is_some() {
                    metadata.collations.internal_with_control(
                        *column,
                        input.qualified_identity_collation(identity),
                        input.qualified_identity_type(identity),
                        control,
                    )?;
                }
            }
            Ok(())
        })
    }
}

fn publish(
    input: &RowSchema,
    selected: &ProductionMap<'_, InternalColumnRef, Alias<'_>>,
    control: &ProductionControl<'_>,
    complete: impl FnOnce(&mut crate::schema::SchemaBuildMetadata) -> Result<(), SQLError>,
) -> Result<RowSchema, SQLError> {
    let mut metadata = super::copy::metadata(input, control)?;
    metadata.internal = SchemaMap::from_produced(
        input
            .index
            .executor_attributes
            .iter()
            .filter(|(column, _)| selected.get(column).is_none())
            .map(|(column, slot)| {
                Ok((
                    control.finish(*column, control.empty_reservation())?,
                    control.finish(*slot, control.empty_reservation())?,
                ))
            })
            .chain(selected.iter().map(|(column, (slot, _, _))| {
                Ok((
                    control.finish(*column, control.empty_reservation())?,
                    control.finish(*slot, control.empty_reservation())?,
                ))
            })),
        control,
    )?;
    metadata.internal_types = SchemaMap::from_produced(
        input
            .index
            .cold
            .executor_attribute_types
            .iter()
            .filter(|(column, _)| selected.get(column).is_none())
            .map(|(column, ty)| {
                Ok((
                    control.finish(*column, control.empty_reservation())?,
                    super::optional_type(ty.as_ref(), control)?,
                ))
            })
            .chain(selected.iter().map(|(column, (_, ty, _))| {
                Ok((
                    control.finish(*column, control.empty_reservation())?,
                    super::optional_type(*ty, control)?,
                ))
            })),
        control,
    )?;
    for (column, (_, ty, oid)) in selected {
        metadata
            .collations
            .internal_with_control(*column, *oid, *ty, control)?;
    }
    complete(&mut metadata)?;
    super::copy::with_metadata(input, metadata, control)
}
