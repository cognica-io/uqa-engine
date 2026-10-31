//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scope composition publishes selected metadata directly into admitted immutable schemas.

use super::{
    copy,
    lease::{boxed_text, SchemaMemory},
    optional_type, parts,
};
use crate::{
    schema::{
        ColumnIdentity, ColumnType, RowSchema, SchemaBuildMetadata, SchemaMap, SchemaSet,
        ScoreSource, NULL_SLOT,
    },
    SQLError,
};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

mod join;
mod outer;
mod qualifier;
mod row_images;

fn inline<T>(value: T, control: &ProductionControl<'_>) -> Result<Produced<T>, SQLError> {
    Ok(control.finish(value, control.empty_reservation())?)
}

fn optional_text(
    value: Option<&str>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Option<Box<str>>>, SQLError> {
    let (value, memory) = match value {
        Some(value) => {
            let (value, memory) = boxed_text(value, control)?.into_parts();
            (Some(value), memory)
        }
        None => (None, control.empty_reservation()),
    };
    Ok(control.finish(value, memory)?)
}

fn identity_types<'a>(
    entries: impl IntoIterator<Item = (&'a ColumnIdentity, Option<&'a ColumnType>)>,
    control: &ProductionControl<'_>,
) -> Result<SchemaMap<ColumnIdentity, Option<ColumnType>>, SQLError> {
    SchemaMap::from_produced(
        entries.into_iter().map(|(identity, ty)| {
            Ok((
                identity.clone_with_control(control)?,
                optional_type(ty, control)?,
            ))
        }),
        control,
    )
}

fn records(
    left: &RowSchema,
    right: &RowSchema,
    control: &ProductionControl<'_>,
) -> Result<SchemaMap<usize, crate::schema::RecordMetadata>, SQLError> {
    SchemaMap::from_produced(
        left.index
            .cold
            .record_fields
            .iter()
            .map(|(slot, record)| (*slot, record))
            .chain(
                right
                    .index
                    .cold
                    .record_fields
                    .iter()
                    .map(|(slot, record)| (left.physical_width() + slot, record)),
            )
            .map(|(slot, record)| Ok((inline(slot, control)?, inline(record.clone(), control)?))),
        control,
    )
}

fn scores<'a>(
    sources: impl IntoIterator<Item = &'a ScoreSource>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<ScoreSource>>, SQLError> {
    let mut result = ProductionVec::new(*control);
    for source in sources {
        let (qualifier, memory) = optional_text(source.qualifier.as_deref(), control)?.into_parts();
        result.push_produced(control.finish(
            ScoreSource {
                qualifier,
                column: source.column,
            },
            memory,
        )?)?;
    }
    Ok(result.finish()?)
}

fn shifted(slot: usize, base: usize) -> usize {
    if slot == NULL_SLOT {
        NULL_SLOT
    } else {
        base + slot
    }
}

struct Layout<'a> {
    columns: ProductionVec<'a, String>,
    identities: ProductionVec<'a, ColumnIdentity>,
    types: ProductionVec<'a, Option<ColumnType>>,
    slots: ProductionVec<'a, usize>,
}

impl<'a> Layout<'a> {
    fn new(control: &ProductionControl<'a>) -> Self {
        Self {
            columns: ProductionVec::new(*control),
            identities: ProductionVec::new(*control),
            types: ProductionVec::new(*control),
            slots: ProductionVec::new(*control),
        }
    }
    fn append(
        &mut self,
        input: &RowSchema,
        base: usize,
        qualifier: Option<&str>,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        for (position, name) in input.columns().iter().enumerate() {
            self.columns.push_produced(control.copy_text(name)?)?;
            self.identities.push_produced(match qualifier {
                Some(qualifier) => {
                    ColumnIdentity::from_parts_with_control(Some(qualifier), name, control)?
                }
                None => input.identities()[position].clone_with_control(control)?,
            })?;
            self.types
                .push_produced(optional_type(input.column_type(position), control)?)?;
            self.slots
                .push_copy(shifted(input.index.slots[position], base))?;
        }
        Ok(())
    }
    fn finish(
        self,
        width: usize,
        metadata: SchemaBuildMetadata,
        control: &ProductionControl<'_>,
    ) -> Result<RowSchema, SQLError> {
        parts::produce(
            self.columns.finish()?,
            self.identities.finish()?,
            self.types.finish()?,
            self.slots.finish()?,
            width,
            metadata,
            control,
        )
    }
}
