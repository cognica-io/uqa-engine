//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    empty_source, MutableQueryPlan, MutableSourcePlan, SQLError, SourceChildSlot, SourcePlan,
};
use uqa_core::memory::ProductionControl;

impl MutableSourcePlan {
    /// Observe source aliases and retained descriptors while rewriting the source's query. The original box stays in the source with an empty placeholder during the callback.
    pub fn map_query_with_parent(
        mut self,
        control: &ProductionControl<'_>,
        map: impl FnOnce(&SourcePlan, MutableQueryPlan) -> Result<MutableQueryPlan, SQLError>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let (query, field) = match &mut self.source {
            SourcePlan::Subquery { body, .. } => (body, "body"),
            SourcePlan::Table {
                executable_query: Some(query),
                ..
            } => (query, "executable_query"),
            SourcePlan::Table {
                executable_query: None,
                ..
            } => return Ok(self),
            _ => {
                return Err(SQLError::Internal(
                    "source has no directly retained query".into(),
                ))
            }
        };
        let input = MutableQueryPlan {
            query: std::mem::replace(query.as_mut(), super::super::queries::empty_query()),
            memory: self.memory.as_mut().and_then(|memory| {
                memory
                    .field_mut(field)
                    .as_mut()
                    .expect("source query box")
                    .field_mut("query")
                    .take()
            }),
        };
        let output = map(&self.source, input)?;
        output.assert_owner(control);
        control.check()?;
        let query = match &mut self.source {
            SourcePlan::Subquery { body, .. } => body,
            SourcePlan::Table {
                executable_query: Some(query),
                ..
            } => query,
            _ => unreachable!("read-only parent cannot change topology"),
        };
        **query = output.query;
        if let Some(memory) = &mut self.memory {
            *memory
                .field_mut(field)
                .as_mut()
                .expect("source query box")
                .field_mut("query") = output.memory;
        }
        Ok(self)
    }

    /// Observe the join's metadata and other input while moving one source child through a rewrite.
    pub fn map_child_with_parent(
        mut self,
        slot: SourceChildSlot,
        control: &ProductionControl<'_>,
        map: impl FnOnce(&SourcePlan, Self) -> Result<Self, SQLError>,
    ) -> Result<Self, SQLError> {
        self.assert_owner(control);
        control.check()?;
        let input = Self {
            source: std::mem::replace(slot.source_mut(&mut self.source), empty_source()),
            memory: self
                .memory
                .as_mut()
                .and_then(|memory| slot.memory_mut(memory).take()),
        };
        let output = map(&self.source, input)?;
        output.assert_owner(control);
        control.check()?;
        *slot.source_mut(&mut self.source) = output.source;
        if let Some(memory) = &mut self.memory {
            *slot.memory_mut(memory) = output.memory;
        }
        Ok(self)
    }
}
