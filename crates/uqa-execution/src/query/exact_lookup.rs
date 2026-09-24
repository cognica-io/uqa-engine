//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact-key probes over the caller's selected document, index and private row views.

use super::table_read::TableRead;
use crate::{serializable::SerializableRelationRead, storage_errors::storage_error};
use std::collections::BTreeMap;
use uqa_core::{DocId, PostingList, Predicate, Value};
use uqa_sql::{ast::ColumnDef, SQLError};
use uqa_storage::{document_store::Document, DocumentStore, StoredDocument};

#[derive(Clone, Copy)]
pub enum FieldPresence {
    Required,
    MissingIsNull,
}

/// Command adapters may retain cached exact keys; snapshot overlays expose their selected rows.
pub trait ExactLookupOverlay {
    fn is_empty(&self) -> Result<bool, SQLError>;
    fn masks(&self, doc_id: DocId) -> Result<bool, SQLError>;
    fn find_match(
        &self,
        columns: &[String],
        values: &[Value],
        presence: FieldPresence,
    ) -> Result<Option<DocId>, SQLError>;
}

pub fn matches_fields(
    document: &Document,
    columns: &[String],
    values: &[Value],
    presence: FieldPresence,
) -> Result<bool, SQLError> {
    matches_fields_with_control(
        document,
        columns,
        values,
        presence,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
}

pub(crate) fn matches_fields_with_control(
    document: &Document,
    columns: &[String],
    values: &[Value],
    presence: FieldPresence,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<bool, SQLError> {
    for (column, expected) in columns.iter().zip(values) {
        let actual = document.get(column);
        if matches!(presence, FieldPresence::Required) && actual.is_none() {
            return Ok(false);
        }
        if !uqa_sql::expr::compare_typed_values_with_control(
            actual.unwrap_or(&Value::Null),
            expected,
            control,
        )?
        .is_eq()
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn matches_value(
    actual: Option<&Value>,
    expected: &Value,
    presence: FieldPresence,
) -> Result<bool, SQLError> {
    if matches!(presence, FieldPresence::Required) && actual.is_none() {
        return Ok(false);
    }
    uqa_sql::expr::compare_typed_values_with_control(
        actual.unwrap_or(&Value::Null),
        expected,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
    .map(std::cmp::Ordering::is_eq)
}

impl ExactLookupOverlay for BTreeMap<DocId, Option<StoredDocument>> {
    fn is_empty(&self) -> Result<bool, SQLError> {
        Ok(self.is_empty())
    }
    fn masks(&self, doc_id: DocId) -> Result<bool, SQLError> {
        Ok(self.contains_key(&doc_id))
    }
    fn find_match(
        &self,
        columns: &[String],
        values: &[Value],
        presence: FieldPresence,
    ) -> Result<Option<DocId>, SQLError> {
        for (id, document) in self {
            if let Some(document) = document {
                if matches_fields(document.fields(), columns, values, presence)? {
                    return Ok(Some(*id));
                }
            }
        }
        Ok(None)
    }
}

impl ExactLookupOverlay for super::document_changes::DocumentChanges {
    fn is_empty(&self) -> Result<bool, SQLError> {
        Ok(!self.has_changes())
    }

    fn masks(&self, id: DocId) -> Result<bool, SQLError> {
        Ok(self.contains_change(id))
    }

    fn find_match(
        &self,
        columns: &[String],
        values: &[Value],
        presence: FieldPresence,
    ) -> Result<Option<DocId>, SQLError> {
        for (id, present) in self.changes() {
            if !present {
                continue;
            }
            let mut matches = true;
            for (column, expected) in columns.iter().zip(values) {
                let actual = self
                    .get_field(id, column)
                    .map_err(|error| storage_error("read private exact key", &error))?;
                if !matches_value(actual.as_ref(), expected, presence)? {
                    matches = false;
                    break;
                }
            }
            if matches {
                return Ok(Some(id));
            }
        }
        Ok(None)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IndexConflictProbe {
    Unanswerable,
    NoConflict,
    Conflict(DocId),
}

/// A nonnegative integer primary key uses the existing document identity mapping.
fn primary_key_doc_id(columns: &[ColumnDef], column: &str, value: &Value) -> Option<DocId> {
    let Value::Int(id) = value else {
        return None;
    };
    if *id < 0
        || !columns.iter().any(|candidate| {
            candidate.name == column && candidate.primary_key && candidate.ty.is_integer()
        })
    {
        return None;
    }
    Some(*id as DocId)
}

pub struct ExactLookup<'a> {
    pub table: &'a dyn TableRead,
    pub overlay: &'a dyn ExactLookupOverlay,
    pub read: Option<&'a SerializableRelationRead>,
}

impl ExactLookup<'_> {
    /// A field lookup preserves the document-store distinction between absent and explicitly NULL fields.
    pub fn find_field(&self, field: &str, value: &Value) -> Result<Option<DocId>, SQLError> {
        self.scan(
            &[field.to_string()],
            std::slice::from_ref(value),
            FieldPresence::Required,
        )
    }

    pub fn find_conflict(
        &self,
        schema_columns: &[ColumnDef],
        columns: &[String],
        values: &[Value],
        scan: impl FnMut(&str, &Predicate) -> Result<Option<PostingList>, SQLError>,
    ) -> Result<Option<DocId>, SQLError> {
        if columns.is_empty() || columns.len() != values.len() {
            return Ok(None);
        }
        if columns.len() == 1 {
            if let Some(id) = primary_key_doc_id(schema_columns, &columns[0], &values[0]) {
                if let Some(read) = self.read {
                    read.observe_row(id)?;
                }
                if let Some(id) =
                    self.overlay
                        .find_match(columns, values, FieldPresence::MissingIsNull)?
                {
                    return Ok(Some(id));
                }
                if self.overlay.masks(id)? {
                    return Ok(None);
                }
                return self
                    .table
                    .read_documents()
                    .contains_doc_id(id)
                    .map(|exists| exists.then_some(id))
                    .map_err(|error| storage_error("check conflicting document", &error));
            }
        }
        match self.indexed_conflict(columns, values, scan)? {
            IndexConflictProbe::Conflict(id) => Ok(Some(id)),
            IndexConflictProbe::NoConflict => Ok(None),
            IndexConflictProbe::Unanswerable => {
                self.scan(columns, values, FieldPresence::MissingIsNull)
            }
        }
    }

    /// The first answerable index is authoritative, including an empty result. The supplied index reader records its precise predicate before returning candidates.
    fn indexed_conflict(
        &self,
        columns: &[String],
        values: &[Value],
        mut scan: impl FnMut(&str, &Predicate) -> Result<Option<PostingList>, SQLError>,
    ) -> Result<IndexConflictProbe, SQLError> {
        for (pivot, (column, value)) in columns.iter().zip(values).enumerate() {
            let predicate = if matches!(value, Value::Null) {
                Predicate::IsNull
            } else {
                Predicate::Equals(value.clone())
            };
            let Some(candidates) = scan(column, &predicate)? else {
                continue;
            };
            if let Some(id) =
                self.overlay
                    .find_match(columns, values, FieldPresence::MissingIsNull)?
            {
                return Ok(IndexConflictProbe::Conflict(id));
            }
            let documents = self.table.read_documents();
            for entry in candidates.entries() {
                if self.overlay.masks(entry.doc_id)? {
                    continue;
                }
                if columns.len() > 1 {
                    if let Some(read) = self.read {
                        read.observe_row(entry.doc_id)?;
                    }
                }
                let mut matches = true;
                for (index, (column, expected)) in columns.iter().zip(values).enumerate() {
                    if index == pivot {
                        continue;
                    }
                    let actual = documents
                        .get_field(entry.doc_id, column)
                        .map_err(|error| storage_error("verify conflicting document", &error))?;
                    if !matches_value(actual.as_ref(), expected, FieldPresence::MissingIsNull)? {
                        matches = false;
                        break;
                    }
                }
                if matches {
                    return Ok(IndexConflictProbe::Conflict(entry.doc_id));
                }
            }
            return Ok(IndexConflictProbe::NoConflict);
        }
        Ok(IndexConflictProbe::Unanswerable)
    }

    fn scan(
        &self,
        columns: &[String],
        values: &[Value],
        presence: FieldPresence,
    ) -> Result<Option<DocId>, SQLError> {
        if let Some(read) = self.read {
            read.observe_scan()?;
        }
        if let Some(id) = self.overlay.find_match(columns, values, presence)? {
            return Ok(Some(id));
        }
        let documents = self.table.read_documents();
        let comparison_can_fail = values.iter().any(uqa_sql::expr::value_comparison_can_fail)
            || self.table.column_definitions().iter().any(|column| {
                columns.contains(&column.name)
                    && uqa_sql::expr::type_comparison_can_fail(&column.ty)
            });
        if self.overlay.is_empty()? && !comparison_can_fail {
            return match presence {
                FieldPresence::Required => documents.find_doc_id_by_field(&columns[0], &values[0]),
                FieldPresence::MissingIsNull => documents.find_doc_id_by_fields(columns, values),
            }
            .map_err(|error| storage_error("find matching document", &error));
        }
        let mut after = None;
        loop {
            let ids = documents
                .next_doc_ids(after, crate::DEFAULT_BATCH_SIZE)
                .map_err(|error| storage_error("scan command-visible document fields", &error))?;
            let Some(last) = ids.last().copied() else {
                return Ok(None);
            };
            after = Some(last);
            for id in ids {
                if self.overlay.masks(id)? {
                    continue;
                }
                let mut matches = true;
                for (column, expected) in columns.iter().zip(values) {
                    let actual = documents.get_field(id, column).map_err(|error| {
                        storage_error("read command-visible document field", &error)
                    })?;
                    if !matches_value(actual.as_ref(), expected, presence)? {
                        matches = false;
                        break;
                    }
                }
                if matches {
                    return Ok(Some(id));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
