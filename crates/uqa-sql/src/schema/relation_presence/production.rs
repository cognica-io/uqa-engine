//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{PresenceBindings, RelationPresence};
use crate::{
    schema::{
        production::lease::{boxed_text, SchemaMemory},
        RowSchema, SchemaMap,
    },
    SQLError,
};
use std::sync::Arc;
use uqa_core::memory::ProductionControl;

impl RelationPresence {
    pub(in crate::schema) fn compose_with_control(
        left: &RowSchema,
        right: &RowSchema,
        outer: bool,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let bindings = SchemaMap::from_produced(
            left.whole_row_presences()
                .filter(|(name, _)| outer || !right.has_qualifier(name))
                .chain(
                    right
                        .whole_row_presences()
                        .filter(|(name, _)| !left.has_qualifier(name)),
                )
                .map(|(name, reference)| {
                    Ok((
                        boxed_text(name, control)?,
                        control.finish(reference, control.empty_reservation())?,
                    ))
                }),
            control,
        )?;
        if bindings.is_empty() {
            return Ok(Self::default());
        }
        let memory = control.reserve(size_of::<PresenceBindings>())?;
        Ok(Self(Some(Arc::new(PresenceBindings {
            bindings,
            _memory: SchemaMemory::new(memory),
        }))))
    }
}

impl RowSchema {
    /// Admit the new presence map and complete physical layout before publishing either. Existing nested markers remain attached to their original relation scopes.
    pub fn with_nullable_relations_with_control(
        &self,
        marker: crate::ast::InternalColumnRef,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Self>, SQLError> {
        control.check()?;
        let mut unmarked = false;
        for qualifier in self.relation_qualifiers() {
            control.check()?;
            if self.whole_row_presence(qualifier).is_none() {
                unmarked = true;
                break;
            }
        }
        if !unmarked {
            return Ok(None);
        }
        let bindings = SchemaMap::from_produced(
            self.whole_row_presences()
                .chain(
                    self.relation_qualifiers()
                        .filter(|qualifier| self.whole_row_presence(qualifier).is_none())
                        .map(|qualifier| (qualifier, marker)),
                )
                .map(|(name, reference)| {
                    Ok((
                        boxed_text(name, control)?,
                        control.finish(reference, control.empty_reservation())?,
                    ))
                }),
            control,
        )?;
        let memory = control.reserve(size_of::<PresenceBindings>())?;
        let presence = RelationPresence(Some(Arc::new(PresenceBindings {
            bindings,
            _memory: SchemaMemory::new(memory),
        })));
        let mut result = Self::project_with_sources_with_control(
            self,
            std::iter::empty(),
            &[(
                marker,
                Some(&crate::ColumnType::Boolean),
                crate::schema::ProjectedSlot::Computed {
                    position: 0,
                    collation: None,
                },
            )],
            1,
            true,
            control,
        )?;
        Arc::get_mut(&mut result.index)
            .expect("new nullable layout is not shared")
            .cold
            .row_presence = presence;
        Ok(Some(result))
    }
}
