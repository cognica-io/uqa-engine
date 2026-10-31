//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{optional_type, parts};
use crate::{
    ast::InternalRelationId,
    schema::{RowSchema, SchemaBuildMetadata, SchemaMap},
    ColumnType, SQLError,
};
use uqa_core::memory::{ProductionControl, ProductionVec};

impl RowSchema {
    /// Publish executor-only declared attributes, admitting names, type payloads, indexes and selected collations before construction.
    pub fn with_internal_relation_metadata_with_control<'a>(
        relation: InternalRelationId,
        columns: impl IntoIterator<Item = (Option<&'a ColumnType>, Option<u32>)>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let mut metadata = SchemaBuildMetadata::empty_with_control(control);
        let mut selected = ProductionVec::new(*control);
        for (position, (ty, oid)) in columns.into_iter().enumerate() {
            control.check()?;
            if oid.is_some_and(|oid| oid != 0)
                && !ty.is_some_and(crate::schema::composites::type_is_collatable)
            {
                return Err(SQLError::Internal(
                    "internal VALUES collation requires a collatable attribute type".into(),
                ));
            }
            selected.push_copy(ty)?;
            metadata.collations.internal_with_control(
                relation.column(position),
                oid,
                ty,
                control,
            )?;
        }
        let width = selected.len();
        metadata.internal = SchemaMap::from_produced(
            (0..width).map(|position| {
                Ok((
                    control.finish(relation.column(position), control.empty_reservation())?,
                    control.finish(position, control.empty_reservation())?,
                ))
            }),
            control,
        )?;
        metadata.internal_types = SchemaMap::from_produced(
            selected.iter().enumerate().map(|(position, ty)| {
                Ok((
                    control.finish(relation.column(position), control.empty_reservation())?,
                    optional_type(*ty, control)?,
                ))
            }),
            control,
        )?;
        drop(selected);
        parts::produce(
            control.finish(Vec::new(), control.empty_reservation())?,
            control.finish(Vec::new(), control.empty_reservation())?,
            control.finish(Vec::new(), control.empty_reservation())?,
            control.finish(Vec::new(), control.empty_reservation())?,
            width,
            metadata,
            control,
        )
    }
}
