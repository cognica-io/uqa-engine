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
