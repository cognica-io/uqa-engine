//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Generated recursive names own static lookup metadata independently of visible row slots.

use crate::{
    schema::{ColumnIdentity, RowSchema, SchemaMap, SchemaSet},
    ColumnType, SQLError,
};
use uqa_core::memory::{ProductionControl, ProductionMap};

impl RowSchema {
    pub(crate) fn with_conflicting_virtual_columns_with_control<'a>(
        input: &'a Self,
        fields: impl IntoIterator<Item = (&'a str, Option<&'a ColumnType>)>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let mut generated = ProductionMap::new(*control);
        for (name, ty) in fields {
            if generated.get(&name).is_none() {
                generated.insert_copy(name, ty)?;
            }
        }
        let mut metadata = super::copy::metadata(input, control)?;
        metadata.binding_only = SchemaMap::from_produced(
            input
                .typed_virtual_identities()
                .map(|(identity, ty)| {
                    Ok((
                        identity.clone_with_control(control)?,
                        super::optional_type(ty, control)?,
                    ))
                })
                .chain(
                    generated
                        .iter()
                        .filter(|(name, _)| {
                            !input.index.cold.binding_only.keys().any(|identity| {
                                identity.qualifier().is_none() && identity.column() == **name
                            })
                        })
                        .map(|(name, ty)| {
                            Ok((
                                ColumnIdentity::from_parts_with_control(None, name, control)?,
                                super::optional_type(*ty, control)?,
                            ))
                        }),
                ),
            control,
        )?;
        metadata.extra_ambiguous_unqualified = SchemaSet::from_produced(
            input
                .index
                .ambiguous_unqualified
                .iter()
                .map(|name| super::lease::boxed_text(name, control))
                .chain(
                    generated
                        .iter()
                        .filter(|(name, _)| {
                            input.index.unqualified.contains_key(**name)
                                || input
                                    .index
                                    .aliases
                                    .keys()
                                    .chain(input.index.cold.binding_only.keys())
                                    .any(|identity| {
                                        identity.qualifier().is_none()
                                            && identity.column() == **name
                                    })
                        })
                        .map(|(name, _)| super::lease::boxed_text(name, control)),
                ),
            control,
        )?;
        super::copy::with_metadata(input, metadata, control)
    }
}

impl RowSchema {
    pub fn with_virtual_columns_with_control<'a>(
        input: &'a Self,
        columns: impl IntoIterator<Item = (Option<&'a str>, &'a str, Option<&'a ColumnType>)>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let mut selected = ProductionMap::new(*control);
        for (qualifier, column, ty) in columns {
            let identity = ColumnIdentity::from_parts_with_control(qualifier, column, control)?;
            let exists = if qualifier.is_some() {
                input.has_qualified_identity(&identity)
            } else {
                input.index.ambiguous_unqualified.contains(column)
                    || input.index.unqualified.contains_key(column)
                    || input.index.aliases.contains_key(&*identity)
                    || input.index.cold.binding_only.contains_key(&*identity)
            };
            if !exists && selected.get(&(qualifier, column)).is_none() {
                selected.insert_copy((qualifier, column), ty)?;
            }
        }
        if selected.is_empty() {
            return Ok(input.clone());
        }
        let mut metadata = super::copy::metadata(input, control)?;
        metadata.binding_only = SchemaMap::from_produced(
            input
                .typed_virtual_identities()
                .map(|(identity, ty)| {
                    Ok((
                        identity.clone_with_control(control)?,
                        super::optional_type(ty, control)?,
                    ))
                })
                .chain(selected.iter().map(|((qualifier, column), ty)| {
                    Ok((
                        ColumnIdentity::from_parts_with_control(*qualifier, column, control)?,
                        super::optional_type(*ty, control)?,
                    ))
                })),
            control,
        )?;
        super::copy::with_metadata(input, metadata, control)
    }

    pub(crate) fn has_qualified_identity(&self, identity: &ColumnIdentity) -> bool {
        self.index.qualified.contains_key(identity)
            || self.index.aliases.contains_key(identity)
            || self.index.cold.binding_only.contains_key(identity)
    }

    pub(crate) fn qualified_identity_type(&self, identity: &ColumnIdentity) -> Option<&ColumnType> {
        if self.index.ambiguous_qualified.contains(identity) {
            return None;
        }
        self.index
            .qualified
            .get(identity)
            .and_then(|position| self.column_type(*position))
            .or_else(|| {
                self.index
                    .cold
                    .aliases
                    .get(identity)
                    .and_then(Option::as_ref)
            })
            .or_else(|| {
                self.index
                    .cold
                    .binding_only
                    .get(identity)
                    .and_then(Option::as_ref)
            })
    }
}
