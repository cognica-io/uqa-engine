//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! JOIN output identities select borrowed physical positions and retain independently admitted metadata.

use super::{copy, lease::SchemaMemory, optional_type, parts};
use crate::{
    schema::{
        collations::composition::Selection, ColumnIdentity, RowSchema, SchemaBuildMetadata,
        SchemaMap, SchemaSet, ScoreSource, NULL_SLOT,
    },
    ColumnType, SQLError,
};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

mod metadata;

pub(in crate::schema) type OutputColumn<'a> =
    (&'a str, &'a ColumnIdentity, usize, Option<&'a ColumnType>);
pub(in crate::schema) type OutputAlias<'a> = (&'a ColumnIdentity, usize, Option<&'a ColumnType>);

struct Layout<'a, 'c> {
    input: &'a RowSchema,
    control: ProductionControl<'c>,
    names: ProductionVec<'c, String>,
    identities: ProductionVec<'c, ColumnIdentity>,
    types: ProductionVec<'c, Option<ColumnType>>,
    slots: ProductionVec<'c, usize>,
    hidden: ProductionVec<'c, usize>,
    collations: Selection<'a, 'c>,
}

impl<'a, 'c> Layout<'a, 'c> {
    fn new(input: &'a RowSchema, control: &ProductionControl<'c>) -> Result<Self, SQLError> {
        let mut collations = Selection::new(&input.index.cold.collations, control)?;
        collations.clear_columns(control);
        Ok(Self {
            input,
            control: *control,
            names: ProductionVec::new(*control),
            identities: ProductionVec::new(*control),
            types: ProductionVec::new(*control),
            slots: ProductionVec::new(*control),
            hidden: ProductionVec::new(*control),
            collations,
        })
    }

    fn push(
        &mut self,
        label: &str,
        identity: (Option<&str>, &str),
        slot: usize,
        ty: Option<&ColumnType>,
        collation: Option<u32>,
        hidden: bool,
    ) -> Result<(), SQLError> {
        let control = &self.control;
        let position = self.names.len();
        self.names.push_produced(control.copy_text(label)?)?;
        self.identities
            .push_produced(ColumnIdentity::from_parts_with_control(
                identity.0, identity.1, control,
            )?)?;
        self.types.push_produced(optional_type(ty, control)?)?;
        self.slots.push_copy(slot)?;
        self.collations.column(position, collation, ty)?;
        if hidden {
            self.hidden.push_copy(position)?;
        }
        Ok(())
    }

    fn finish(
        self,
        width: usize,
        aliases: &[OutputAlias<'a>],
        keep_input_aliases: bool,
        rebound_score: Option<&str>,
        control: &ProductionControl<'_>,
    ) -> Result<RowSchema, SQLError> {
        let hidden = SchemaSet::from_produced(
            self.hidden
                .iter()
                .map(|position| inline(*position, control)),
            control,
        )?;
        let metadata = metadata::produce(
            self.input,
            aliases,
            self.collations.finish(control)?,
            hidden,
            keep_input_aliases,
            rebound_score,
            control,
        )?;
        parts::produce(
            self.names.finish()?,
            self.identities.finish()?,
            self.types.finish()?,
            self.slots.finish()?,
            width,
            metadata,
            control,
        )
    }
}

pub(in crate::schema) fn physical<'a>(
    input: &'a RowSchema,
    columns: impl IntoIterator<Item = OutputColumn<'a>>,
    aliases: impl IntoIterator<Item = OutputAlias<'a>>,
    physical_width: usize,
    control: &ProductionControl<'_>,
) -> Result<RowSchema, SQLError> {
    control.check()?;
    let mut layout = Layout::new(input, control)?;
    for (label, identity, physical, ty) in columns {
        control.check()?;
        let hidden = input
            .index
            .cold
            .wildcard_hidden
            .iter()
            .any(|logical| input.slot(*logical) == Some(physical));
        layout.push(
            label,
            (identity.qualifier(), identity.column()),
            if physical < physical_width {
                physical
            } else {
                NULL_SLOT
            },
            ty,
            input.inherited_physical_collation(physical),
            hidden,
        )?;
    }
    let mut selected = ProductionVec::new(*control);
    for (identity, physical, ty) in aliases {
        control.check()?;
        layout.collations.alias(
            (identity.qualifier(), identity.column()),
            input.inherited_physical_collation(physical),
            ty,
        )?;
        selected.push_copy((
            identity,
            if physical < physical_width {
                physical
            } else {
                NULL_SLOT
            },
            ty,
        ))?;
    }
    layout.finish(physical_width, &selected, true, None, control)
}

impl RowSchema {
    pub(crate) fn remap_join_alias_with_control(
        input: &Self,
        qualifier: &str,
        columns: &[String],
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let mut layout = Layout::new(input, control)?;
        layout.collations.clear_lookup_aliases(control);
        for (position, name) in columns.iter().enumerate() {
            layout.push(
                name,
                (Some(qualifier), name),
                input.slot(position).unwrap_or(NULL_SLOT),
                input.column_type(position),
                input.inherited_column_collation(position),
                input.index.cold.wildcard_hidden.contains(&position),
            )?;
        }
        let schema = layout.finish(input.physical_width(), &[], false, Some(qualifier), control)?;
        if schema.is_empty() {
            Self::with_relation_qualifier_with_control(&schema, qualifier, control)
        } else {
            Ok(schema)
        }
    }

    pub(crate) fn canonical_join_alias_with_control(
        input: &Self,
        qualifier: &str,
        columns: &[String],
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let metadata = SchemaBuildMetadata {
            lateral_restrictions: Self::requalified_column_names_with_control(input, control)?,
            ..SchemaBuildMetadata::empty_with_control(control)
        };
        super::columns_with_metadata(
            Some(qualifier),
            None,
            columns
                .iter()
                .enumerate()
                .map(|(position, name)| (name.as_str(), input.column_type(position), None, None)),
            false,
            metadata,
            control,
        )
    }
}

fn inline<T>(value: T, control: &ProductionControl<'_>) -> Result<Produced<T>, SQLError> {
    Ok(control.finish(value, control.empty_reservation())?)
}

#[cfg(test)]
mod tests;
