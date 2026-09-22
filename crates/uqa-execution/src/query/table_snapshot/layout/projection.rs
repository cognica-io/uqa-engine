//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Map projected fields without decoding unrelated row payloads.

use super::RowLayout;
use uqa_core::{DocId, Value};
use uqa_storage::{DocumentStore, StorageBackendError, StorageBackendResult};

enum Slot<'a> {
    Source(usize),
    Constant(&'a Value),
}

pub(in crate::query::table_snapshot) struct RowProjection<'a> {
    pub sources: Vec<&'a str>,
    slots: Vec<Slot<'a>>,
    ambiguous_nulls: Vec<usize>,
}

impl RowProjection<'_> {
    pub fn values<'a>(&'a self, values: &[&'a Value]) -> Vec<&'a Value> {
        self.slots
            .iter()
            .map(|slot| match slot {
                Slot::Source(index) => values[*index],
                Slot::Constant(value) => *value,
            })
            .collect()
    }

    pub fn only_sources(&self) -> bool {
        self.sources.len() == self.slots.len()
    }

    pub fn needs_presence(&self, values: &[&Value]) -> bool {
        self.ambiguous_nulls
            .iter()
            .any(|index| matches!(values[*index], Value::Null))
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
    ) -> Option<RowProjection<'a>> {
        let mut sources = Vec::new();
        let mut slots = Vec::with_capacity(fields.len());
        let mut ambiguous_nulls = Vec::new();
        for field in fields {
            let original_slot = self.source.iter().any(|(name, _)| name == field);
            let source = if let Some(column) = self.columns.iter().find(|c| c.name == *field) {
                // Owned projections distinguish absent fields from explicit NULL before applying a missing value, and evaluate generated dependencies.
                if column.generated.is_some() {
                    return None;
                }
                match self
                    .source_name(field)
                    .or_else(|| (!original_slot).then_some(*field))
                {
                    Some(_) if column.missing_value.is_some() => return None,
                    Some(source) => Some(source),
                    None => {
                        slots.push(Slot::Constant(
                            column.missing_value.as_ref().unwrap_or(&Value::Null),
                        ));
                        continue;
                    }
                }
            } else if original_slot {
                None
            } else {
                Some(*field)
            };
            if let Some(source) = source {
                // A missing renamed source preserves an undeclared target field, whereas an explicit NULL overwrites it.
                if source != *field && !original_slot {
                    ambiguous_nulls.push(sources.len());
                }
                slots.push(Slot::Source(sources.len()));
                sources.push(source);
            } else {
                slots.push(Slot::Constant(&Value::Null));
            }
        }
        Some(RowProjection {
            sources,
            slots,
            ambiguous_nulls,
        })
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
            if let Some(value) = source.get_field(id, name)? {
                return Ok(Some(value));
            }
        }
        if !self.source.iter().any(|(name, _)| name == field) {
            if let Some(value) = source.get_field(id, field)? {
                return Ok(Some(value));
            }
        }
        if column.generated.is_some() {
            return source
                .get_stored(id)?
                .map(|row| {
                    self.adapt_base(row)
                        .map(|mut row| row.fields_mut().remove(field))
                        .map_err(Self::error)
                })
                .transpose()
                .map(Option::flatten);
        }
        Ok(source
            .contains_doc_id(id)?
            .then(|| column.missing_value.clone().unwrap_or(Value::Null)))
    }
}
