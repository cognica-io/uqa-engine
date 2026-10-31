//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    identity_types, inline, optional_text, optional_type, records, scores, shifted, ColumnIdentity,
    Layout, ProductionControl, RowSchema, SQLError, SchemaBuildMetadata, SchemaMap, SchemaMemory,
    SchemaSet, NULL_SLOT,
};

impl RowSchema {
    /// Compose both physical layouts while retaining each selected namespace and metadata owner.
    pub fn join_with_control(
        left: &Self,
        right: &Self,
        extra_columns: impl IntoIterator<Item = String>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        Self::join_column_inputs_with_control(left, right, extra_columns, control)
    }

    /// Borrow compatibility NULL-extension names without constructing an intermediate name buffer.
    pub fn join_borrowed_columns_with_control<'a>(
        left: &Self,
        right: &Self,
        extra_columns: impl IntoIterator<Item = &'a str>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        Self::join_column_inputs_with_control(left, right, extra_columns, control)
    }

    fn join_column_inputs_with_control(
        left: &Self,
        right: &Self,
        extra_columns: impl IntoIterator<Item = impl AsRef<str>>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        let mut layout = Layout::new(control);
        layout.append(left, 0, None, control)?;
        layout.append(right, left.physical_width(), None, control)?;
        for name in extra_columns {
            control.check()?;
            let name = name.as_ref();
            if !layout.columns.iter().any(|column| column == name) {
                layout.columns.push_produced(control.copy_text(name)?)?;
                layout
                    .identities
                    .push_produced(ColumnIdentity::from_parts_with_control(
                        None, name, control,
                    )?)?;
                layout.types.push_produced(inline(None, control)?)?;
                layout.slots.push_copy(NULL_SLOT)?;
            }
        }
        for column in right.index.executor_attributes.keys() {
            control.check()?;
            assert!(
                !left.index.executor_attributes.contains_key(column),
                "duplicate internal relation attribute in joined row"
            );
        }
        for column in right.index.cold.executor_attribute_types.keys() {
            control.check()?;
            assert!(
                !left
                    .index
                    .cold
                    .executor_attribute_types
                    .contains_key(column),
                "duplicate internal relation attribute type in joined row"
            );
        }
        let metadata = metadata(left, right, control)?;
        layout.finish(
            left.physical_width() + right.physical_width(),
            metadata,
            control,
        )
    }
}

fn metadata(
    left: &RowSchema,
    right: &RowSchema,
    control: &ProductionControl<'_>,
) -> Result<SchemaBuildMetadata, SQLError> {
    let (score_sources, memory) = scores(
        left.index
            .cold
            .score_sources
            .iter()
            .chain(&right.index.cold.score_sources),
        control,
    )?
    .into_parts();
    Ok(SchemaBuildMetadata {
        collations: crate::schema::Collations::joined_with_control(left, right, control)?,
        record_fields: records(left, right, control)?,
        aliases: joined_aliases(left, right, control)?,
        alias_types: identity_types(
            left.index
                .cold
                .aliases
                .iter()
                .filter(|(name, _)| !right.index.cold.aliases.contains_key(*name))
                .chain(right.index.cold.aliases.iter())
                .map(|(name, ty)| (name, ty.as_ref())),
            control,
        )?,
        internal: joined_internal_slots(left, right, control)?,
        internal_types: SchemaMap::from_produced(
            left.index
                .cold
                .executor_attribute_types
                .iter()
                .chain(&right.index.cold.executor_attribute_types)
                .map(|(column, ty)| {
                    Ok((
                        inline(*column, control)?,
                        optional_type(ty.as_ref(), control)?,
                    ))
                }),
            control,
        )?,
        score_sources,
        wildcard_hidden: SchemaSet::from_produced(
            left.index
                .cold
                .wildcard_hidden
                .iter()
                .copied()
                .chain(
                    right
                        .index
                        .cold
                        .wildcard_hidden
                        .iter()
                        .map(|position| left.len() + position),
                )
                .map(|position| inline(position, control)),
            control,
        )?,
        binding_only: identity_types(
            left.index
                .cold
                .binding_only
                .iter()
                .filter(|(name, _)| !right.index.cold.binding_only.contains_key(*name))
                .chain(right.index.cold.binding_only.iter())
                .map(|(name, ty)| (name, ty.as_ref())),
            control,
        )?,
        row_types: crate::schema::relation_types::RelationRowTypes::compose_with_control(
            left, right, false, control,
        )?,
        row_presence: crate::schema::relation_presence::RelationPresence::compose_with_control(
            left, right, false, control,
        )?,
        lateral_restrictions: RowSchema::joined_diagnostic_names_with_control(
            left, right, control,
        )?,
        open_qualifiers: SchemaSet::from_produced(
            left.index
                .cold
                .open_qualifiers
                .iter()
                .chain(&right.index.cold.open_qualifiers)
                .map(|name| optional_text(name.as_deref(), control)),
            control,
        )?,
        memory: SchemaMemory::new(memory),
        ..SchemaBuildMetadata::empty_with_control(control)
    })
}

fn joined_aliases(
    left: &RowSchema,
    right: &RowSchema,
    control: &ProductionControl<'_>,
) -> Result<SchemaMap<super::ColumnIdentity, usize>, SQLError> {
    SchemaMap::from_produced(
        left.index
            .aliases
            .iter()
            .filter(|(name, _)| !right.index.aliases.contains_key(*name))
            .map(|(name, slot)| (name, *slot))
            .chain(
                right
                    .index
                    .aliases
                    .iter()
                    .map(|(name, slot)| (name, shifted(*slot, left.physical_width()))),
            )
            .map(|(name, slot)| Ok((name.clone_with_control(control)?, inline(slot, control)?))),
        control,
    )
}

fn joined_internal_slots(
    left: &RowSchema,
    right: &RowSchema,
    control: &ProductionControl<'_>,
) -> Result<SchemaMap<crate::ast::InternalColumnRef, usize>, SQLError> {
    SchemaMap::from_produced(
        left.index
            .executor_attributes
            .iter()
            .map(|(column, slot)| (*column, *slot))
            .chain(
                right
                    .index
                    .executor_attributes
                    .iter()
                    .map(|(column, slot)| (*column, shifted(*slot, left.physical_width()))),
            )
            .map(|(column, slot)| Ok((inline(column, control)?, inline(slot, control)?))),
        control,
    )
}
