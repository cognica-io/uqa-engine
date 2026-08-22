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
        let base = input.physical_width();
        for (offset, (name, ty)) in names.iter().enumerate() {
            let slot = base + offset;
            if let Some(position) = columns.iter().position(|column| column == name) {
                slots[position] = slot;
                identities[position] = ColumnIdentity::unqualified(name);
                types[position].clone_from(ty);
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
        if self.len() != target.len() {
            return Err(ExecError::Other(format!(
                "cannot relayout {} logical columns as {} logical columns",
                self.len(),
                target.len()
            )));
        }

        let mut source_slots = vec![None; target.physical_width()];
        let mut assign = |target_slot: usize, source_slot: usize| -> ExecResult<()> {
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
        };

        for logical in 0..target.len() {
            assign(target.index.slots[logical], self.index.slots[logical])?;
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
            assign(*target_slot, source_slot)?;
        }

        let source_slots = source_slots
            .into_iter()
            .map(|slot| slot.unwrap_or(NULL_SLOT))
            .collect::<Vec<_>>();
        Ok(row.project_slots(&source_slots))
    }
}
