//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Logical-to-physical layout inspection and wildcard expansion.

use super::{ColumnIdentity, ColumnType, HashSet, RowSchema, NULL_SLOT};

impl RowSchema {
    pub fn columns(&self) -> &[String] {
        &self.index.columns
    }

    pub fn len(&self) -> usize {
        self.index.columns.len()
    }

    pub fn is_empty(&self) -> bool {
        self.index.columns.is_empty()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, String> {
        self.index.columns.iter()
    }

    /// Static SQL type at one logical output position.
    pub fn column_type(&self, logical: usize) -> Option<&ColumnType> {
        self.index
            .cold
            .columns
            .get(logical)
            .and_then(Option::as_ref)
    }

    /// Static SQL types aligned with [`Self::columns`].
    pub fn column_types(&self) -> &[Option<ColumnType>] {
        &self.index.cold.columns
    }

    /// Structured SQL identities aligned with [`Self::columns`].
    pub fn identities(&self) -> &[ColumnIdentity] {
        &self.index.identities
    }

    pub fn identity(&self, logical: usize) -> Option<&ColumnIdentity> {
        self.index.identities.get(logical)
    }

    pub fn public_name(&self, logical: usize) -> Option<&str> {
        self.identity(logical).map(ColumnIdentity::column)
    }

    pub fn position(&self, name: &str) -> Option<usize> {
        self.index.exact.get(name).copied()
    }

    /// Resolve one visible unqualified SQL identity to its logical position. Ambiguous names deliberately do not select an arbitrary owner.
    pub fn unqualified_position(&self, column: &str) -> Option<usize> {
        if self.index.ambiguous_unqualified.contains(column) {
            return None;
        }
        self.index.unqualified.get(column).copied()
    }

    /// Resolve one visible qualified identity to its logical position.
    pub fn qualified_position(&self, qualifier: &str, column: &str) -> Option<usize> {
        let identity = ColumnIdentity::qualified(qualifier, column);
        if self.index.ambiguous_qualified.contains(&identity) {
            return None;
        }
        self.index.qualified.get(&identity).copied()
    }

    pub fn physical_width(&self) -> usize {
        self.index.physical_width
    }

    /// Resolve one logical output position to its flattened physical slot.
    pub fn physical_slot(&self, logical: usize) -> Option<usize> {
        self.slot(logical)
    }

    /// Resolve a structured SQL identity, including a hidden JOIN USING alias, to its flattened physical slot. Ambiguous identities deliberately return `None` rather than selecting an arbitrary source value.
    pub fn physical_slot_for_identity(&self, identity: &ColumnIdentity) -> Option<usize> {
        match identity.qualifier() {
            Some(qualifier) => self.qualified_slot(qualifier, identity.column()),
            None => self.column_slot(identity.column()),
        }
    }

    pub(crate) fn slot(&self, logical: usize) -> Option<usize> {
        self.index
            .slots
            .get(logical)
            .copied()
            .filter(|slot| *slot != NULL_SLOT)
    }

    pub(super) fn exact_slot(&self, name: &str) -> Option<usize> {
        self.index
            .exact
            .get(name)
            .and_then(|logical| self.slot(*logical))
            .filter(|slot| *slot != NULL_SLOT)
    }

    pub(super) fn exact_type(&self, name: &str) -> Option<&ColumnType> {
        self.index
            .exact
            .get(name)
            .and_then(|logical| self.column_type(*logical))
    }

    pub(super) fn column_slot(&self, name: &str) -> Option<usize> {
        if self.index.ambiguous_unqualified.contains(name) {
            return None;
        }
        self.index
            .unqualified
            .get(name)
            .and_then(|logical| self.slot(*logical))
            .or_else(|| {
                self.index
                    .aliases
                    .get(&ColumnIdentity::unqualified(name))
                    .copied()
            })
            .filter(|slot| *slot != NULL_SLOT)
    }

    /// Physical projection layout for `qualifier.*` in relation-column order. Hidden identities introduced by `JOIN ... USING` remain selectable, so each side's wildcard retains its own merged-column value.
    pub fn qualified_star_layout(
        &self,
        qualifier: &str,
    ) -> Vec<(String, usize, Option<ColumnType>)> {
        self.qualified_star_position_layout(qualifier)
            .into_iter()
            .map(|(column, _, slot, ty)| (column, slot, ty))
            .collect()
    }

    /// Bound layout for `qualifier.*`. Visible columns retain their logical positions; hidden aliases such as the suppressed side of `JOIN ... USING` expose only their physical slot.
    pub fn qualified_star_position_layout(
        &self,
        qualifier: &str,
    ) -> Vec<(String, Option<usize>, usize, Option<ColumnType>)> {
        let mut entries = Vec::new();
        let mut visible_layout = HashSet::new();
        for (logical, identity) in self.identities().iter().enumerate() {
            if identity.qualifier() == Some(qualifier) {
                let slot = self.slot(logical).unwrap_or(NULL_SLOT);
                visible_layout.insert((identity.clone(), slot));
                entries.push((
                    identity.clone(),
                    Some(logical),
                    slot,
                    self.column_type(logical).cloned(),
                ));
            }
        }
        for (identity, slot) in &self.index.aliases {
            if identity.qualifier() == Some(qualifier)
                && !visible_layout.contains(&(identity.clone(), *slot))
            {
                entries.push((
                    identity.clone(),
                    None,
                    *slot,
                    self.index.cold.aliases.get(identity).cloned().flatten(),
                ));
            }
        }
        entries.sort_by_key(|(_, _, slot, _)| *slot);
        entries
            .into_iter()
            .map(|(identity, logical, slot, ty)| (identity.column().to_string(), logical, slot, ty))
            .collect()
    }

    pub(super) fn qualified_slot(&self, qualifier: &str, column: &str) -> Option<usize> {
        let identity = ColumnIdentity::qualified(qualifier, column);
        if self.index.ambiguous_qualified.contains(&identity) {
            return None;
        }
        self.index
            .qualified
            .get(&identity)
            .and_then(|logical| self.slot(*logical))
            .or_else(|| self.index.aliases.get(&identity).copied())
            .filter(|slot| *slot != NULL_SLOT)
    }
}
