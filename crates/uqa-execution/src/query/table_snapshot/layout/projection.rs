//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Map projected fields without decoding unrelated row payloads.

use super::RowLayout;
use uqa_core::{memory::BudgetedVec, DocId, Value};
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
        self.control.check()?;
        let mut sources = BudgetedVec::new(self.control.memory());
        let mut slots = BudgetedVec::new(self.control.memory());
        for field in fields {
            self.control.check()?;
            let original_slot = self.source.iter().any(|(name, _)| name == field);
            let column = self.columns.iter().find(|c| c.name == *field);
            let source = if let Some(column) = column {
                // Generated production retains its separate expression-evaluation boundary.
                if column.generated.is_some() {
                    return Ok(None);
                }
                match self
                    .source_name(field)
                    .or_else(|| (!original_slot).then_some(*field))
                {
                    Some(source) => Some(source),
                    None => {
                        slots.push(Slot::Constant(
                            column.missing_value.as_ref().unwrap_or(&Value::Null),
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
                let default = column.and_then(|column| column.missing_value.as_ref());
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
    ) -> StorageBackendResult<Option<Value>> {
        let Some(column) = self.columns.iter().find(|column| column.name == field) else {
            return if self.source.iter().any(|(name, _)| name == field) {
                Ok(None)
            } else {
                source.get_field(id, field)
            };
        };
        if let Some(name) = self.source_name(field) {
            let value = source.get_field(id, name)?;
            self.control.check()?;
            if let Some(value) = value {
                return Ok(Some(value));
            }
        }
        if !self.source.iter().any(|(name, _)| name == field) {
            let value = source.get_field(id, field)?;
            self.control.check()?;
            if let Some(value) = value {
                return Ok(Some(value));
            }
        }
        if column.generated.is_some() {
            return source
                .get_stored(id)?
                .map(|row| {
                    (|| {
                        let mut row = self.complete_defaults(self.remap_base(row)?)?;
                        crate::query::generated::materialize_missing_generated_column(
                            &self.columns,
                            row.fields_mut(),
                            field,
                        )?;
                        self.control.cancellation().check()?;
                        Ok(row.fields_mut().remove(field))
                    })()
                    .map_err(Self::error)
                })
                .transpose()
                .map(Option::flatten);
        }
        let present = source.contains_doc_id(id)?;
        self.control.check()?;
        Ok(present.then(|| column.missing_value.clone().unwrap_or(Value::Null)))
    }
}
