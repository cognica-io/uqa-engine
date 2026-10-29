//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Correlated namespaces select physical aliases, ambiguity and virtual metadata before copying.

use super::{
    boxed_text, copy, inline, optional_text, optional_type, records, scores, shifted,
    ColumnIdentity, ColumnType, Layout, ProductionControl, RowSchema, SQLError, SchemaMap,
    SchemaMemory, SchemaSet, NULL_SLOT,
};
use crate::schema::{collations::composition::Selection as Collations, identity_lookup::ColumnKey};
use uqa_core::memory::ProductionMap;

type Key<'a> = (Option<&'a str>, &'a str);
struct Aliases<'a, 'c> {
    slots: ProductionMap<'c, Key<'a>, usize>,
    types: ProductionMap<'c, Key<'a>, Option<&'a ColumnType>>,
    ambiguous_unqualified: ProductionMap<'c, &'a str, ()>,
    ambiguous_qualified: ProductionMap<'c, Key<'a>, ()>,
    virtuals: ProductionMap<'c, Key<'a>, Option<&'a ColumnType>>,
    collations: Collations<'a, 'c>,
}

impl<'a, 'c> Aliases<'a, 'c> {
    fn new(input: &'a RowSchema, control: &ProductionControl<'c>) -> Result<Self, SQLError> {
        let mut aliases = Self {
            slots: ProductionMap::new(*control),
            types: ProductionMap::new(*control),
            ambiguous_unqualified: ProductionMap::new(*control),
            ambiguous_qualified: ProductionMap::new(*control),
            virtuals: ProductionMap::new(*control),
            collations: Collations::new(&input.index.cold.collations, control)?,
        };
        for (identity, slot) in &input.index.aliases {
            aliases.slots.insert_copy(key(identity), *slot)?;
        }
        for (identity, ty) in &input.index.cold.aliases {
            aliases.types.insert_copy(key(identity), ty.as_ref())?;
        }
        for name in &input.index.ambiguous_unqualified {
            aliases
                .ambiguous_unqualified
                .insert_copy(name.as_ref(), ())?;
        }
        for identity in &input.index.ambiguous_qualified {
            aliases.ambiguous_qualified.insert_copy(key(identity), ())?;
        }
        for (identity, ty) in &input.index.cold.binding_only {
            aliases.virtuals.insert_copy(key(identity), ty.as_ref())?;
        }
        Ok(aliases)
    }

    fn overlay(
        &mut self,
        input: &'a RowSchema,
        outer: &'a RowSchema,
        control: &ProductionControl<'c>,
    ) -> Result<(), SQLError> {
        self.overlay_qualified(input, outer, control)?;
        self.overlay_unqualified(input, outer, control)?;
        self.overlay_virtuals(input, outer, control)?;
        Ok(())
    }
    fn overlay_qualified(
        &mut self,
        input: &'a RowSchema,
        outer: &'a RowSchema,
        control: &ProductionControl<'c>,
    ) -> Result<(), SQLError> {
        let mut identities = ProductionMap::new(*control);
        for identity in outer
            .index
            .qualified
            .keys()
            .chain(
                outer
                    .index
                    .aliases
                    .keys()
                    .filter(|key| key.qualifier().is_some()),
            )
            .chain(outer.index.ambiguous_qualified.iter())
        {
            identities.insert_copy(key(identity), identity)?;
        }
        for ((qualifier, column), identity) in &identities {
            control.check()?;
            let qualifier = qualifier.expect("qualified outer key");
            if input
                .relation_qualifiers()
                .any(|candidate| candidate == qualifier)
            {
                continue;
            }
            let selected = (Some(qualifier), *column);
            if outer.index.ambiguous_qualified.contains(*identity) {
                self.slots.remove(&selected);
                self.types.remove(&selected);
                self.ambiguous_qualified.insert_copy(selected, ())?;
            } else {
                let slot = outer
                    .index
                    .qualified
                    .get(*identity)
                    .map(|position| outer.index.slots[*position])
                    .or_else(|| outer.index.aliases.get(*identity).copied())
                    .unwrap_or(NULL_SLOT);
                let ty = outer.qualified_identity_type(identity);
                self.slots
                    .insert_copy(selected, shifted(slot, input.physical_width()))?;
                self.types.insert_copy(selected, ty)?;
                self.collations.alias(
                    selected,
                    outer.qualified_collation(qualifier, column),
                    ty,
                )?;
            }
        }
        Ok(())
    }

    fn overlay_unqualified(
        &mut self,
        input: &'a RowSchema,
        outer: &'a RowSchema,
        control: &ProductionControl<'c>,
    ) -> Result<(), SQLError> {
        let mut unqualified = ProductionMap::new(*control);
        for name in outer
            .index
            .unqualified
            .keys()
            .map(std::convert::AsRef::as_ref)
            .chain(
                outer
                    .index
                    .aliases
                    .keys()
                    .filter(|identity| identity.qualifier().is_none())
                    .map(ColumnIdentity::column),
            )
            .chain(
                outer
                    .index
                    .ambiguous_unqualified
                    .iter()
                    .map(std::convert::AsRef::as_ref),
            )
        {
            unqualified.insert_copy(name, ())?;
        }
        for (name, ()) in &unqualified {
            let selected = (None, *name);
            let lookup = &selected as &dyn ColumnKey;
            if input.index.unqualified.contains_key(*name)
                || input.index.aliases.contains_key(lookup)
            {
                continue;
            }
            if outer.column_is_ambiguous(name) {
                self.slots.remove(&selected);
                self.types.remove(&selected);
                self.ambiguous_unqualified.insert_copy(name, ())?;
            } else {
                let slot = outer
                    .index
                    .unqualified
                    .get(*name)
                    .map(|position| outer.index.slots[*position])
                    .or_else(|| outer.index.aliases.get(lookup).copied())
                    .unwrap_or(NULL_SLOT);
                let ty = outer
                    .index
                    .unqualified
                    .get(*name)
                    .and_then(|position| outer.column_type(*position))
                    .or_else(|| {
                        outer
                            .index
                            .cold
                            .aliases
                            .get(lookup)
                            .and_then(Option::as_ref)
                    })
                    .or_else(|| {
                        outer
                            .index
                            .cold
                            .binding_only
                            .get(lookup)
                            .and_then(Option::as_ref)
                    });
                self.slots
                    .insert_copy(selected, shifted(slot, input.physical_width()))?;
                self.types.insert_copy(selected, ty)?;
                self.collations
                    .alias(selected, outer.collation_of(name), ty)?;
            }
        }
        Ok(())
    }

    fn overlay_virtuals(
        &mut self,
        input: &'a RowSchema,
        outer: &'a RowSchema,
        control: &ProductionControl<'c>,
    ) -> Result<(), SQLError> {
        for (identity, ty) in outer.typed_virtual_identities() {
            control.check()?;
            let selected = key(identity);
            if selected.0.is_some_and(|qualifier| {
                input
                    .relation_qualifiers()
                    .any(|candidate| candidate == qualifier)
            }) {
                continue;
            }
            let visible = if selected.0.is_some() {
                input.index.qualified.contains_key(identity)
                    || self.slots.get(&selected).is_some()
                    || self.virtuals.get(&selected).is_some()
            } else {
                self.ambiguous_unqualified.get(selected.1).is_some()
                    || input.index.unqualified.contains_key(selected.1)
                    || self.slots.get(&selected).is_some()
                    || self.virtuals.get(&selected).is_some()
            };
            if !visible {
                self.virtuals.insert_copy(selected, ty)?;
            }
            if !input.index.cold.binding_only.contains_key(identity) {
                if let Some(ty) = self.virtuals.get(&selected) {
                    self.collations.virtual_column(
                        selected,
                        outer.virtual_collation(identity),
                        *ty,
                    )?;
                }
            }
        }
        Ok(())
    }

    fn install_aliases(
        &self,
        metadata: &mut super::SchemaBuildMetadata,
        control: &ProductionControl<'c>,
    ) -> Result<(), SQLError> {
        metadata.aliases = SchemaMap::from_produced(
            self.slots.iter().map(|((qualifier, column), slot)| {
                Ok((
                    ColumnIdentity::from_parts_with_control(*qualifier, column, control)?,
                    inline(*slot, control)?,
                ))
            }),
            control,
        )?;
        metadata.alias_types = SchemaMap::from_produced(
            self.types.iter().map(|((qualifier, column), ty)| {
                Ok((
                    ColumnIdentity::from_parts_with_control(*qualifier, column, control)?,
                    optional_type(*ty, control)?,
                ))
            }),
            control,
        )?;
        metadata.binding_only = SchemaMap::from_produced(
            self.virtuals.iter().map(|((qualifier, column), ty)| {
                Ok((
                    ColumnIdentity::from_parts_with_control(*qualifier, column, control)?,
                    optional_type(*ty, control)?,
                ))
            }),
            control,
        )?;
        metadata.extra_ambiguous_unqualified = SchemaSet::from_produced(
            self.ambiguous_unqualified
                .iter()
                .map(|(name, ())| boxed_text(name, control)),
            control,
        )?;
        metadata.extra_ambiguous_qualified = SchemaSet::from_produced(
            self.ambiguous_qualified
                .iter()
                .map(|((qualifier, column), ())| {
                    ColumnIdentity::from_parts_with_control(*qualifier, column, control)
                        .map_err(Into::into)
                }),
            control,
        )?;
        Ok(())
    }
}

fn key(identity: &ColumnIdentity) -> Key<'_> {
    (identity.qualifier(), identity.column())
}

impl RowSchema {
    /// Overlay an outer physical namespace with the same shadowing and ambiguity rules as lexical binding.
    pub fn with_outer_schema_with_control(
        input: &Self,
        outer: &Self,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        let mut selected = Aliases::new(input, control)?;
        selected.overlay(input, outer, control)?;
        let mut metadata = copy::metadata(input, control)?;
        selected.install_aliases(&mut metadata, control)?;
        metadata.internal = SchemaMap::from_produced(
            input
                .index
                .executor_attributes
                .iter()
                .filter(|(column, _)| !outer.index.executor_attributes.contains_key(*column))
                .map(|(column, slot)| (*column, *slot))
                .chain(
                    outer
                        .index
                        .executor_attributes
                        .iter()
                        .map(|(column, slot)| (*column, shifted(*slot, input.physical_width()))),
                )
                .map(|(column, slot)| Ok((inline(column, control)?, inline(slot, control)?))),
            control,
        )?;
        for (column, ty) in &outer.index.cold.executor_attribute_types {
            control.check()?;
            if let Some(existing) = input.index.cold.executor_attribute_types.get(column) {
                assert_eq!(
                    existing, ty,
                    "structural identity type changed across correlated scopes"
                );
            }
            selected.collations.internal(
                *column,
                outer.internal_collation(*column),
                ty.as_ref(),
            )?;
        }
        metadata.internal_types =
            SchemaMap::from_produced(
                input
                    .index
                    .cold
                    .executor_attribute_types
                    .iter()
                    .chain(outer.index.cold.executor_attribute_types.iter().filter(
                        |(column, _)| {
                            !input
                                .index
                                .cold
                                .executor_attribute_types
                                .contains_key(*column)
                        },
                    ))
                    .map(|(column, ty)| {
                        Ok((
                            inline(*column, control)?,
                            optional_type(ty.as_ref(), control)?,
                        ))
                    }),
                control,
            )?;
        metadata.collations = selected.collations.finish(control)?;
        metadata.record_fields = records(input, outer, control)?;
        let (score_sources, memory) = scores(
            input
                .index
                .cold
                .score_sources
                .iter()
                .chain(&outer.index.cold.score_sources),
            control,
        )?
        .into_parts();
        metadata.score_sources = score_sources;
        metadata.memory = SchemaMemory::new(memory);
        metadata.open_qualifiers = outer_open_qualifiers(input, outer, control)?;
        metadata.row_types = crate::schema::relation_types::RelationRowTypes::compose_with_control(
            input, outer, true, control,
        )?;
        metadata.row_presence =
            crate::schema::relation_presence::RelationPresence::compose_with_control(
                input, outer, true, control,
            )?;
        metadata.lateral_restrictions =
            Self::outer_lateral_restrictions_with_control(input, outer, control)?;
        metadata.local_width = Some(
            input
                .index
                .cold
                .local_width
                .unwrap_or_else(|| input.physical_width()),
        );
        let mut layout = Layout::new(control);
        layout.append(input, 0, None, control)?;
        layout.finish(
            input.physical_width() + outer.physical_width(),
            metadata,
            control,
        )
    }
}

fn outer_open_qualifiers(
    input: &RowSchema,
    outer: &RowSchema,
    control: &ProductionControl<'_>,
) -> Result<SchemaSet<Option<Box<str>>>, SQLError> {
    SchemaSet::from_produced(
        input
            .index
            .cold
            .open_qualifiers
            .iter()
            .chain(outer.index.cold.open_qualifiers.iter().filter(|name| {
                name.as_deref()
                    .is_none_or(|name| !input.has_qualifier(name))
            }))
            .map(|name| optional_text(name.as_deref(), control)),
        control,
    )
}
