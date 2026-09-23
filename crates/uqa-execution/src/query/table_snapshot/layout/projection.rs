//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Map projected fields without decoding unrelated row payloads.

use super::RowLayout;
use uqa_core::{
    memory::{BudgetedVec, MemoryReservation},
    DocId, Value,
};
use uqa_storage::{
    read_control::StorageReadControl, DocumentStore, StorageBackendError, StorageBackendResult,
};

enum Slot<'a> {
    Source(usize),
    Missing {
        source: usize,
        alternate: Option<usize>,
        default: &'a Value,
    },
    Constant(&'a Value),
}

pub(in crate::query::table_snapshot) struct RowProjection<'a> {
    pub sources: BudgetedVec<&'a str>,
    slots: BudgetedVec<Slot<'a>>,
    control: StorageReadControl,
}

impl RowProjection<'_> {
    pub fn values<'a>(
        &'a self,
        values: &[&'a Value],
        present: &[bool],
    ) -> StorageBackendResult<BudgetedVec<&'a Value>> {
        self.control.check()?;
        let mut projected = BudgetedVec::new(self.control.memory());
        projected.reserve(self.slots.len())?;
        for slot in self.slots.iter() {
            self.control.check()?;
            projected.push(match slot {
                Slot::Source(index) => values[*index],
                Slot::Missing {
                    source,
                    alternate,
                    default,
                } => {
                    if present[*source] {
                        values[*source]
                    } else if let Some(alternate) = alternate.filter(|index| present[*index]) {
                        values[alternate]
                    } else {
                        default
                    }
                }
                Slot::Constant(value) => *value,
            })?;
        }
        Ok(projected)
    }

    pub fn shared_sources(&self) -> StorageBackendResult<Option<BudgetedVec<&str>>> {
        self.control.check()?;
        let mut sources = BudgetedVec::new(self.control.memory());
        for slot in self.slots.iter() {
            self.control.check()?;
            let source = match slot {
                Slot::Source(source) | Slot::Missing { source, .. } => *source,
                Slot::Constant(_) => return Ok(None),
            };
            sources.push(self.sources[source])?;
        }
        Ok(Some(sources))
    }

    pub fn needs_shared_fallback(&self, values: &[&Value]) -> bool {
        self.slots.iter().zip(values).any(|(slot, value)| {
            matches!(slot, Slot::Missing { .. }) && matches!(value, Value::Null)
        })
    }

    pub fn needs_presence(&self) -> bool {
        self.slots
            .iter()
            .any(|slot| matches!(slot, Slot::Missing { .. }))
    }
}

impl RowLayout {
    pub(super) fn error(error: uqa_sql::SQLError) -> StorageBackendError {
        StorageBackendError::backend("query row layout", error)
    }

    fn source_name(&self, field: &str) -> Option<&str> {
        self.source
            .iter()
            .find(|(_, target)| target.as_deref() == Some(field))
            .map(|(source, _)| source.as_str())
    }

    pub(in crate::query::table_snapshot) fn projection<'a>(
        &'a self,
        fields: &'a [&str],
    ) -> StorageBackendResult<Option<RowProjection<'a>>> {
        self.project_fields(fields, false)
    }

    pub(super) fn generated_inputs<'a>(
        &'a self,
        fields: &'a [&str],
    ) -> StorageBackendResult<RowProjection<'a>> {
        self.project_fields(fields, true).map(|projection| {
            projection.expect("stored generated inputs do not evaluate another expression")
        })
    }

    fn project_fields<'a>(
        &'a self,
        fields: &'a [&str],
        stored_generated: bool,
    ) -> StorageBackendResult<Option<RowProjection<'a>>> {
        self.control.check()?;
        let mut sources = BudgetedVec::new(self.control.memory());
        let mut slots = BudgetedVec::new(self.control.memory());
        for field in fields {
            self.control.check()?;
            let original_slot = self.source.iter().any(|(name, _)| name == field);
            let column = self.columns.iter().find(|c| c.name == *field);
            let source = if let Some(column) = column {
                // Generated production retains its separate expression-evaluation boundary.
                if column.generated.is_some() && !stored_generated {
                    return Ok(None);
                }
                match self
                    .source_name(field)
                    .or_else(|| (!original_slot).then_some(*field))
                {
                    Some(source) => Some(source),
                    None => {
                        slots.push(Slot::Constant(
                            column
                                .missing_value
                                .as_ref()
                                .filter(|_| column.generated.is_none())
                                .unwrap_or(&Value::Null),
                        ))?;
                        continue;
                    }
                }
            } else if original_slot {
                None
            } else {
                Some(*field)
            };
            if let Some(source) = source {
                let index = sources.len();
                sources.push(source)?;
                let alternate = if source != *field && !original_slot {
                    let alternate = sources.len();
                    sources.push(*field)?;
                    Some(alternate)
                } else {
                    None
                };
                let default = column
                    .filter(|column| column.generated.is_none())
                    .and_then(|column| column.missing_value.as_ref());
                if alternate.is_some() || default.is_some() {
                    slots.push(Slot::Missing {
                        source: index,
                        alternate,
                        default: default.unwrap_or(&Value::Null),
                    })?;
                } else {
                    slots.push(Slot::Source(index))?;
                }
            } else {
                slots.push(Slot::Constant(&Value::Null))?;
            }
        }
        Ok(Some(RowProjection {
            sources,
            slots,
            control: self.control.clone(),
        }))
    }

    pub(in crate::query::table_snapshot) fn field_is_declared(&self, field: &str) -> bool {
        self.columns.iter().any(|column| column.name == field)
    }

    pub(in crate::query::table_snapshot) fn field_is_removed(&self, field: &str) -> bool {
        !self.field_is_declared(field) && self.source.iter().any(|(name, _)| name == field)
    }

    pub(in crate::query::table_snapshot) fn base_field(
        &self,
        source: &dyn DocumentStore,
        id: DocId,
        field: &str,
        memory: &mut MemoryReservation,
    ) -> StorageBackendResult<Option<Value>> {
        let Some(column) = self.columns.iter().find(|column| column.name == field) else {
            return if self.source.iter().any(|(name, _)| name == field) {
                Ok(None)
            } else {
                self.copy_source_field(source, id, field, memory)
            };
        };
        if let Some(name) = self.source_name(field) {
            let value = self.copy_source_field(source, id, name, memory)?;
            self.control.check()?;
            if let Some(value) = value {
                return Ok(Some(value));
            }
        }
        if !self.source.iter().any(|(name, _)| name == field) {
            let value = self.copy_source_field(source, id, field, memory)?;
            self.control.check()?;
            if let Some(value) = value {
                return Ok(Some(value));
            }
        }
        if column.generated.is_some() {
            return self.generated_field(source, id, column, memory);
        }
        let present = source.contains_doc_id(id)?;
        self.control.check()?;
        if !present {
            return Ok(None);
        }
        let copied = self.copy_default(column.missing_value.as_ref())?;
        self.control.check()?;
        let (value, value_memory) = copied.into_parts();
        memory.absorb(value_memory);
        Ok(Some(value))
    }

    fn copy_source_field(
        &self,
        source: &dyn DocumentStore,
        id: DocId,
        field: &str,
        memory: &mut MemoryReservation,
    ) -> StorageBackendResult<Option<Value>> {
        let value =
            uqa_storage::document_store::read_selected_field(source, id, field, &self.control)?;
        self.control.check()?;
        Ok(value.map(|value| {
            let (value, reservation) = value.into_parts();
            memory.absorb(reservation);
            value
        }))
    }
}
