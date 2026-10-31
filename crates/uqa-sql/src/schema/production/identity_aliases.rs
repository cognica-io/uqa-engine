//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Hidden logical aliases retain their own selected names, types, collations and schema layout.

use super::{copy, optional_type};
use crate::{
    schema::{ColumnIdentity, RowSchema, SchemaMap, NULL_SLOT},
    SQLError,
};
use uqa_core::memory::{ProductionControl, ProductionMap};

impl RowSchema {
    pub fn with_identity_aliases_with_control<'a>(
        input: &'a Self,
        aliases: impl IntoIterator<Item = (Option<&'a str>, &'a str, usize)>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let mut selected = ProductionMap::new(*control);
        let mut collations = crate::schema::collations::composition::Selection::new(
            &input.index.cold.collations,
            control,
        )?;
        for (qualifier, column, logical) in aliases {
            control.check()?;
            selected.insert_copy((qualifier, column), logical)?;
            collations.alias(
                (qualifier, column),
                input.inherited_column_collation(logical),
                input.column_type(logical),
            )?;
        }
        let aliases = SchemaMap::from_produced(
            input
                .index
                .aliases
                .iter()
                .map(|(identity, slot)| {
                    Ok((
                        identity.clone_with_control(control)?,
                        control.finish(*slot, control.empty_reservation())?,
                    ))
                })
                .chain(selected.iter().map(|((qualifier, column), logical)| {
                    Ok((
                        ColumnIdentity::from_parts_with_control(*qualifier, column, control)?,
                        control.finish(
                            input.slot(*logical).unwrap_or(NULL_SLOT),
                            control.empty_reservation(),
                        )?,
                    ))
                })),
            control,
        )?;
        let alias_types = SchemaMap::from_produced(
            input
                .index
                .cold
                .aliases
                .iter()
                .map(|(identity, ty)| {
                    Ok((
                        identity.clone_with_control(control)?,
                        optional_type(ty.as_ref(), control)?,
                    ))
                })
                .chain(selected.iter().map(|((qualifier, column), logical)| {
                    Ok((
                        ColumnIdentity::from_parts_with_control(*qualifier, column, control)?,
                        optional_type(input.column_type(*logical), control)?,
                    ))
                })),
            control,
        )?;
        let metadata = crate::schema::SchemaBuildMetadata {
            local_width: None,
            ..copy::metadata_with_identity_maps(
                input,
                collations.finish(control)?,
                aliases,
                alias_types,
                control,
            )?
        };
        copy::with_metadata(input, metadata, control)
    }
}
