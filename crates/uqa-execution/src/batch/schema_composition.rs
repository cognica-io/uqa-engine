//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Schema append, join, view, and physical relayout operations.

use super::{
    ColumnIdentity, ColumnType, ExecError, ExecResult, PhysicalRow, PhysicalRowView, RowSchema,
    SchemaBuildMetadata, NULL_SLOT,
};

impl RowSchema {
    /// Append freshly-computed values to an existing physical row. Reusing an
    /// existing output name replaces its logical slot just like map insertion.
    pub fn append(input: &Self, names: &[String]) -> Self {
        let columns = names
            .iter()
            .cloned()
            .map(|name| (name, None))
            .collect::<Vec<_>>();
        Self::append_typed(input, &columns)
    }

    /// Append freshly computed values with static SQL output types.
    pub fn append_typed(input: &Self, names: &[(String, Option<ColumnType>)]) -> Self {
        let mut columns = input.columns().to_vec();
        let mut identities = input.identities().to_vec();
        let mut types = input.column_types().to_vec();
        let mut slots = input.index.slots.to_vec();
        let mut wildcard_hidden = input.index.cold.wildcard_hidden.clone();
        let base = input.physical_width();
        for (offset, (name, ty)) in names.iter().enumerate() {
            let slot = base + offset;
            if let Some(position) = columns.iter().position(|column| column == name) {
                slots[position] = slot;
                identities[position] = ColumnIdentity::unqualified(name);
                types[position].clone_from(ty);
                wildcard_hidden.remove(&position);
            } else {
                columns.push(name.clone());
                identities.push(ColumnIdentity::unqualified(name));
                types.push(ty.clone());
                slots.push(slot);
            }
        }
        Self::from_typed_parts_with_aliases_and_exact_precedence(
            columns,
            identities,
            types,
            slots,
            base + names.len(),
            SchemaBuildMetadata {
                aliases: input.index.aliases.clone(),
                alias_types: input.index.cold.aliases.clone(),
                internal: input.index.internal.clone(),
                internal_types: input.index.cold.internal.clone(),
                score_sources: input.index.cold.score_sources.clone(),
                wildcard_hidden,
                binding_only: input.index.cold.binding_only.clone(),
                ..SchemaBuildMetadata::default()
            },
        )
    }

    /// Extend the physical row with anonymous values that have no SQL name or
    /// wildcard presence. Callers may attach structured SQL identities or
    /// internal relation attributes to the resulting physical slots.
    pub fn append_hidden_typed(input: &Self, types: &[Option<ColumnType>]) -> Self {
        Self::from_typed_parts_with_aliases_and_exact_precedence(
            input.columns().to_vec(),
            input.identities().to_vec(),
            input.column_types().to_vec(),
            input.index.slots.to_vec(),
            input.physical_width() + types.len(),
            SchemaBuildMetadata {
                aliases: input.index.aliases.clone(),
                alias_types: input.index.cold.aliases.clone(),
                internal: input.index.internal.clone(),
                internal_types: input.index.cold.internal.clone(),
                score_sources: input.index.cold.score_sources.clone(),
                wildcard_hidden: input.index.cold.wildcard_hidden.clone(),
                binding_only: input.index.cold.binding_only.clone(),
                ..SchemaBuildMetadata::default()
            },
        )
    }

    /// Append computed executor attributes under structural identities while
    /// keeping them out of the SQL name and wildcard namespaces.
    pub fn append_internal_typed(
        input: &Self,
        columns: &[(uqa_sql::ast::InternalColumnRef, Option<ColumnType>)],
    ) -> Self {
        let base = input.physical_width();
        let mut internal = input.index.internal.clone();
        let mut internal_types = input.index.cold.internal.clone();
        for (offset, (column, ty)) in columns.iter().enumerate() {
            internal.insert(*column, base + offset);
            internal_types.insert(*column, ty.clone());
        }
        Self::from_typed_parts_with_aliases_and_exact_precedence(
            input.columns().to_vec(),
            input.identities().to_vec(),
            input.column_types().to_vec(),
            input.index.slots.to_vec(),
            base + columns.len(),
            SchemaBuildMetadata {
                aliases: input.index.aliases.clone(),
                alias_types: input.index.cold.aliases.clone(),
                internal,
                internal_types,
                score_sources: input.index.cold.score_sources.clone(),
                wildcard_hidden: input.index.cold.wildcard_hidden.clone(),
                binding_only: input.index.cold.binding_only.clone(),
                ..SchemaBuildMetadata::default()
            },
        )
    }

    /// Compose two child layouts while retaining duplicate logical labels.
    /// Qualified and positional resolution can then distinguish both input
    /// slots without copying either value fragment.
    pub fn join(
        left: &Self,
        right: &Self,
        extra_columns: impl IntoIterator<Item = String>,
    ) -> Self {
        let mut columns = left.columns().to_vec();
        let mut identities = left.identities().to_vec();
        let mut types = left.column_types().to_vec();
        let mut slots = left.index.slots.to_vec();
        let right_base = left.physical_width();
        let mut aliases = left.index.aliases.clone();
        let mut alias_types = left.index.cold.aliases.clone();
        let mut internal = left.index.internal.clone();
        let mut internal_types = left.index.cold.internal.clone();
        let mut score_sources = left.index.cold.score_sources.clone();
        let mut wildcard_hidden = left.index.cold.wildcard_hidden.clone();
        let mut binding_only = left.index.cold.binding_only.clone();
        aliases.extend(right.index.aliases.iter().map(|(name, slot)| {
            (
                name.clone(),
                if *slot == NULL_SLOT {
                    NULL_SLOT
                } else {
                    right_base + *slot
                },
            )
        }));
        alias_types.extend(
            right
                .index
                .cold
                .aliases
                .iter()
                .map(|(name, ty)| (name.clone(), ty.clone())),
        );
        for (column, slot) in &right.index.internal {
            let shifted = if *slot == NULL_SLOT {
                NULL_SLOT
            } else {
                right_base + *slot
            };
            assert!(
                internal.insert(*column, shifted).is_none(),
                "duplicate internal relation attribute in joined row"
            );
        }
        for (column, ty) in &right.index.cold.internal {
            assert!(
                internal_types.insert(*column, ty.clone()).is_none(),
                "duplicate internal relation attribute type in joined row"
            );
        }
        score_sources.extend(right.index.cold.score_sources.iter().cloned());
        wildcard_hidden.extend(
            right
                .index
                .cold
                .wildcard_hidden
                .iter()
                .map(|position| left.len() + *position),
        );
        binding_only.extend(
            right
                .index
                .cold
                .binding_only
                .iter()
                .map(|(identity, ty)| (identity.clone(), ty.clone())),
        );
        for (right_logical, column) in right.columns().iter().enumerate() {
            let slot = right
                .slot(right_logical)
                .map_or(NULL_SLOT, |slot| right_base + slot);
            columns.push(column.clone());
            identities.push(right.identities()[right_logical].clone());
            types.push(right.column_type(right_logical).cloned());
            slots.push(slot);
        }
        for column in extra_columns {
            if !columns.contains(&column) {
                identities.push(ColumnIdentity::unqualified(column.clone()));
                columns.push(column);
                types.push(None);
                slots.push(NULL_SLOT);
            }
        }
        Self::from_typed_parts_with_aliases_and_exact_precedence(
            columns,
            identities,
            types,
            slots,
            left.physical_width() + right.physical_width(),
            SchemaBuildMetadata {
                aliases,
                alias_types,
                internal,
                internal_types,
                score_sources,
                wildcard_hidden,
                binding_only,
                ..SchemaBuildMetadata::default()
            },
        )
    }

    pub fn view<'a>(&'a self, row: &'a PhysicalRow) -> PhysicalRowView<'a> {
        PhysicalRowView { schema: self, row }
    }

    /// Re-express a row emitted under this schema in `target`'s complete physical layout without cloning any values. Visible columns are matched by logical position; hidden lookup aliases are matched by their structured identity. This is used when two equivalent operator pipelines expose the same logical row through different physical slot arrangements.
    pub fn relayout_physical_row(
        &self,
        row: PhysicalRow,
        target: &Self,
    ) -> ExecResult<PhysicalRow> {
        fn assign(
            source_slots: &mut [Option<usize>],
            target_slot: usize,
            source_slot: usize,
        ) -> ExecResult<()> {
            if target_slot == NULL_SLOT {
                return Ok(());
            }
            match source_slots[target_slot] {
                Some(existing) if existing != source_slot => Err(ExecError::Other(format!(
                    "physical relayout maps target slot {target_slot} to both source slots {existing} and {source_slot}"
                ))),
                Some(_) => Ok(()),
                None => {
                    source_slots[target_slot] = Some(source_slot);
                    Ok(())
                }
            }
        }

        if self.len() != target.len() {
            return Err(ExecError::Other(format!(
                "cannot relayout {} logical columns as {} logical columns",
                self.len(),
                target.len()
            )));
        }

        let mut source_slots = vec![None; target.physical_width()];

        for logical in 0..target.len() {
            assign(
                &mut source_slots,
                target.index.slots[logical],
                self.index.slots[logical],
            )?;
        }

        for (identity, target_slot) in &target.index.aliases {
            if *target_slot == NULL_SLOT {
                continue;
            }
            let mut matching_slots = self
                .index
                .identities
                .iter()
                .enumerate()
                .filter_map(|(logical, candidate)| {
                    (candidate == identity).then_some(self.index.slots[logical])
                })
                .chain(self.index.aliases.get(identity).copied())
                .collect::<Vec<_>>();
            matching_slots.sort_unstable();
            matching_slots.dedup();
            let source_slot = match matching_slots.as_slice() {
                [source_slot] => *source_slot,
                [] => {
                    return Err(ExecError::Other(format!(
                        "physical relayout source is missing lookup identity `{identity:?}`"
                    )))
                }
                _ => {
                    return Err(ExecError::Other(format!(
                        "physical relayout source has ambiguous lookup identity `{identity:?}`"
                    )))
                }
            };
            assign(&mut source_slots, *target_slot, source_slot)?;
        }

        for (column, target_slot) in &target.index.internal {
            if *target_slot == NULL_SLOT {
                continue;
            }
            // An internal target entry may be another structural identity for a slot already mapped through the public target list. A rebuilt EvalPlanQual subtree receives fresh internal relation IDs, but its visible resno layout remains the same; the existing slot assignment is therefore already authoritative.
            if source_slots[*target_slot].is_some() {
                continue;
            }
            let source_slot = self
                .internal_slot(*column)
                .or_else(|| {
                    target
                        .index
                        .cold
                        .score_sources
                        .iter()
                        .find(|source| source.column == *column)
                        .map(|source| source.qualifier.as_deref())
                        .and_then(|qualifier| self.score_source_slot(qualifier))
                })
                .ok_or_else(|| {
                    ExecError::Other(format!(
                        "physical relayout source is missing internal relation attribute `{column:?}`"
                    ))
                })?;
            assign(&mut source_slots, *target_slot, source_slot)?;
        }

        let source_slots = source_slots
            .into_iter()
            .map(|slot| slot.unwrap_or(NULL_SLOT))
            .collect::<Vec<_>>();
        Ok(row.project_slots(&source_slots))
    }
}
