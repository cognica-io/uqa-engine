//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{RelationRowTypes, RelationRows};
use crate::{
    schema::{
        production::lease::{boxed_text, SchemaMemory},
        SchemaMap,
    },
    SQLError,
};
use std::sync::Arc;
use uqa_core::memory::ProductionControl;

impl RelationRowTypes {
    pub(in crate::schema) fn anonymous_with_control(
        qualifier: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        let types = SchemaMap::from_produced(
            std::iter::once_with(|| {
                Ok((
                    boxed_text(qualifier, control)?,
                    control.finish(None, control.empty_reservation())?,
                ))
            }),
            control,
        )?;
        let source = boxed_text(qualifier, control)?;
        let header = control.reserve(size_of::<RelationRows>())?;
        let (source, memory) = source.into_parts();
        let rows = RelationRows {
            types,
            source: Some(source),
            _memory: SchemaMemory::new(control.combine(header, memory)),
        };
        Ok(Self(Some(Arc::new(rows))))
    }
}

impl RelationRowTypes {
    pub(in crate::schema) fn compose_with_control(
        left: &crate::RowSchema,
        right: &crate::RowSchema,
        outer: bool,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let left_rows = left.index.cold.row_types.0.as_deref();
        let right_rows = right.index.cold.row_types.0.as_deref();
        let types = SchemaMap::from_produced(
            left_rows
                .into_iter()
                .flat_map(|rows| rows.types.iter())
                .filter(|(name, _)| outer || !right.has_qualifier(name))
                .chain(
                    right_rows
                        .into_iter()
                        .flat_map(|rows| rows.types.iter())
                        .filter(|(name, _)| !left.has_qualifier(name)),
                )
                .map(|(name, ty)| {
                    Ok((
                        boxed_text(name, control)?,
                        crate::schema::production::optional_type(ty.as_ref(), control)?,
                    ))
                }),
            control,
        )?;
        // An outer namespace preserves the identity of the complete local row.
        let source = if outer {
            left_rows.and_then(|rows| rows.source.as_deref())
        } else {
            None
        };
        Self::from_parts_with_control(types, source, control)
    }

    pub(in crate::schema) fn requalified_with_control(
        &self,
        input: &crate::RowSchema,
        qualifier: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let source = self
            .0
            .as_deref()
            .and_then(|rows| rows.source.as_deref().map(|source| (rows, source)));
        let selected = source.filter(|(_, source)| {
            !input
                .identities()
                .iter()
                .enumerate()
                .any(|(position, identity)| {
                    input.wildcard_position_visible(position)
                        && identity.qualifier() != Some(*source)
                })
        });
        let Some((rows, source)) = selected else {
            return if input.is_empty() {
                Self::anonymous_with_control(qualifier, control)
            } else {
                Ok(Self::default())
            };
        };
        let types = SchemaMap::from_produced(
            std::iter::once_with(|| {
                Ok((
                    boxed_text(qualifier, control)?,
                    crate::schema::production::optional_type(rows.types[source].as_ref(), control)?,
                ))
            }),
            control,
        )?;
        Self::from_parts_with_control(types, Some(qualifier), control)
    }

    fn from_parts_with_control(
        types: SchemaMap<Box<str>, Option<crate::ColumnType>>,
        source: Option<&str>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        if types.is_empty() {
            return Ok(Self::default());
        }
        let source = source
            .map(|source| boxed_text(source, control))
            .transpose()?;
        let header = control.reserve(size_of::<RelationRows>())?;
        let (source, memory) = match source {
            Some(source) => {
                let (source, memory) = source.into_parts();
                (Some(source), memory)
            }
            None => (None, control.empty_reservation()),
        };
        Ok(Self(Some(Arc::new(RelationRows {
            types,
            source,
            _memory: SchemaMemory::new(control.combine(header, memory)),
        }))))
    }
}

impl crate::RowSchema {
    pub fn with_relation_row_type_with_control(
        mut self,
        qualifier: &str,
        ty: &crate::ColumnType,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        if !matches!(ty, crate::ColumnType::Composite(_))
            || (!self.is_empty() && !self.has_qualifier(qualifier))
        {
            return Err(SQLError::Internal(
                "a relation row type requires its named composite and source namespace".into(),
            ));
        }
        let previous = self.index.cold.row_types.0.as_deref();
        let types = SchemaMap::from_produced(
            previous
                .into_iter()
                .flat_map(|rows| rows.types.iter())
                .filter(|(name, _)| name.as_ref() != qualifier)
                .map(|(name, ty)| {
                    Ok((
                        boxed_text(name, control)?,
                        crate::schema::production::optional_type(ty.as_ref(), control)?,
                    ))
                })
                .chain(std::iter::once_with(|| {
                    Ok((
                        boxed_text(qualifier, control)?,
                        crate::schema::production::optional_type(Some(ty), control)?,
                    ))
                })),
            control,
        )?;
        let source = (types.len() == 1).then_some(qualifier);
        let row_types = RelationRowTypes::from_parts_with_control(types, source, control)?;
        if let Some(index) = Arc::get_mut(&mut self.index) {
            index.cold.row_types = row_types;
            control.check()?;
            return Ok(self);
        }
        let mut metadata = crate::schema::production::copy::metadata(&self, control)?;
        metadata.row_types = row_types;
        crate::schema::production::copy::with_metadata(&self, metadata, control)
    }
}
