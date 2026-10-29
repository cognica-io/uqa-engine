//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{RelationRowTypes, RelationRows};
use crate::{
    schema::{
        production::lease::{boxed_text, SchemaMemory},
        SchemaMap,
    },
    SQLError,
};
use std::sync::Arc;
use uqa_core::memory::ProductionControl;

impl RelationRowTypes {
    pub(in crate::schema) fn anonymous_with_control(
        qualifier: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        let types = SchemaMap::from_produced(
            std::iter::once_with(|| {
                Ok((
                    boxed_text(qualifier, control)?,
                    control.finish(None, control.empty_reservation())?,
                ))
            }),
            control,
        )?;
        let source = boxed_text(qualifier, control)?;
        let header = control.reserve(size_of::<RelationRows>())?;
        let (source, memory) = source.into_parts();
        let rows = RelationRows {
            types,
            source: Some(source),
            _memory: SchemaMemory::new(control.combine(header, memory)),
        };
        Ok(Self(Some(Arc::new(rows))))
    }
}
