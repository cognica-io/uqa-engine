//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Whole-row NULL identity is independent of the values of its fields.

use super::{Arc, ColumnType, RowSchema, SchemaLayoutError, SchemaLayoutResult, SchemaMap};
use crate::ast::InternalColumnRef;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct RelationPresence(Option<Arc<PresenceBindings>>);

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct PresenceBindings {
    bindings: SchemaMap<Box<str>, InternalColumnRef>,
    _memory: super::production::lease::SchemaMemory,
}

impl std::ops::Deref for PresenceBindings {
    type Target = SchemaMap<Box<str>, InternalColumnRef>;
    fn deref(&self) -> &Self::Target {
        &self.bindings
    }
}

impl std::ops::DerefMut for PresenceBindings {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.bindings
    }
}

mod production;

impl RelationPresence {
    fn insert(&mut self, qualifier: &str, marker: InternalColumnRef) {
        let bindings = self
            .0
            .get_or_insert_with(|| Arc::new(PresenceBindings::default()));
        Arc::make_mut(bindings).insert(qualifier.into(), marker);
    }

    pub(super) fn retaining(&self, keep: impl Fn(InternalColumnRef) -> bool) -> Self {
        let Some(bindings) = &self.0 else {
            return Self::default();
        };
        if bindings.values().all(|marker| keep(*marker)) {
            return self.clone();
        }
        let retained: SchemaMap<_, _> = bindings
            .iter()
            .filter(|(_, marker)| keep(**marker))
            .map(|(qualifier, marker)| (qualifier.clone(), *marker))
            .collect();
        Self((!retained.is_empty()).then(|| {
            Arc::new(PresenceBindings {
                bindings: retained,
                ..PresenceBindings::default()
            })
        }))
    }

    pub(super) fn joined(left: &RowSchema, right: &RowSchema) -> Self {
        let mut result = Self::default();
        for (source, other) in [(left, right), (right, left)] {
            for (qualifier, marker) in source.whole_row_presences() {
                if !other.has_qualifier(qualifier) {
                    result.insert(qualifier, marker);
                }
            }
        }
        result
    }

    pub(super) fn with_outer(input: &RowSchema, outer: &RowSchema) -> Self {
        let mut result = input.index.cold.row_presence.clone();
        for (qualifier, marker) in outer.whole_row_presences() {
            if !input.has_qualifier(qualifier) {
                result.insert(qualifier, marker);
            }
        }
        result
    }
}

impl RowSchema {
    pub(super) fn relayout_presence_slot(
        &self,
        target: &Self,
        column: InternalColumnRef,
    ) -> Option<usize> {
        target
            .whole_row_presences()
            .find_map(|(qualifier, marker)| {
                (marker == column)
                    .then(|| self.whole_row_presence(qualifier))
                    .flatten()
                    .and_then(|marker| self.internal_slot(marker))
            })
    }

    /// The Boolean executor attribute which distinguishes an existing relation row from its NULL extension. An unmarked relation is always present in this scope.
    pub fn whole_row_presence(&self, qualifier: &str) -> Option<InternalColumnRef> {
        self.index
            .cold
            .row_presence
            .0
            .as_ref()?
            .get(qualifier)
            .copied()
    }

    /// Presence bindings share the ordinary internal-attribute layout and have no SQL column names.
    pub fn whole_row_presences(&self) -> impl Iterator<Item = (&str, InternalColumnRef)> {
        self.index.cold.row_presence.0.iter().flat_map(|bindings| {
            bindings
                .iter()
                .map(|(qualifier, marker)| (qualifier.as_ref(), *marker))
        })
    }

    /// Add one physical marker for the input's still-unmarked relation scopes. Existing inner-join NULL identities keep their original markers. The caller appends TRUE to produced rows; ordinary outer-join NULL extension clears the marker along with the fields.
    pub fn with_nullable_relations(&self, marker: InternalColumnRef) -> Option<Self> {
        self.with_nullable_relations_with_control(
            marker,
            &uqa_core::memory::ProductionControl::uncontrolled(),
        )
        .expect("ordinary nullable relation layout")
    }

    /// Restore validated presence bindings after a physical layout is decoded. The marker must be a real Boolean internal attribute; duplicate namespaces are malformed input.
    pub fn with_whole_row_presences(
        mut self,
        bindings: impl IntoIterator<Item = (String, InternalColumnRef)>,
    ) -> SchemaLayoutResult<Self> {
        let mut presence = RelationPresence::default();
        for (qualifier, marker) in bindings {
            if self.internal_slot(marker).is_none()
                || self.internal_type(marker) != Some(&ColumnType::Boolean)
            {
                return Err(SchemaLayoutError(
                    "whole-row presence requires a Boolean internal attribute".into(),
                ));
            }
            if presence
                .0
                .as_ref()
                .is_some_and(|bindings| bindings.contains_key(qualifier.as_str()))
            {
                return Err(SchemaLayoutError(format!(
                    "duplicate whole-row presence qualifier `{qualifier}`"
                )));
            }
            presence.insert(&qualifier, marker);
        }
        Arc::make_mut(&mut self.index).cold.row_presence = presence;
        Ok(self)
    }
}

#[cfg(test)]
mod tests;
