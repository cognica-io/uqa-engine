//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reuse selected document projections while merging base and private identities through the shared bounded cursor.

use super::{DocId, SQLError, SourceContext};
use crate::query::block::context::QueryDocumentRead;
use crate::query::document_changes::{DocumentChanges, VisibleDocumentIds};
use crate::query::table_read::TableRead;
use crate::serializable::SerializableScan;
use crate::storage_errors::storage_error;
use std::sync::Arc;
use uqa_core::memory::{BudgetedVec, MemoryBudget};
use uqa_core::Value;
use uqa_storage::read_control::StorageReadControl;

pub(super) struct ScalarRows<'a> {
    documents: &'a dyn QueryDocumentRead,
    table: &'a str,
    source: Arc<dyn TableRead>,
    changes: DocumentChanges,
    control: StorageReadControl,
    serializable: SerializableScan,
    candidates: Option<std::vec::IntoIter<DocId>>,
    after: Option<DocId>,
    page: BudgetedVec<DocId>,
    position: usize,
    columns: Vec<String>,
    projected: bool,
    metadata: bool,
    table_oid: Option<Value>,
    origin: (Arc<str>, Arc<str>),
}

impl<'a> ScalarRows<'a> {
    #[expect(
        clippy::too_many_arguments,
        reason = "binds the selected row projection and snapshot without exposing an engine"
    )]
    pub(super) fn new<S: Clone + 'static>(
        context: &SourceContext<'a, S>,
        table: &'a str,
        qualifier: &str,
        source: Arc<dyn TableRead>,
        columns: Vec<String>,
        projected: bool,
        metadata: bool,
        table_oid: Option<Value>,
        candidates: Option<Vec<DocId>>,
        command_visible: bool,
    ) -> Result<Self, SQLError> {
        // Identity and provider decode scratch have a fixed allowance independent of relation cardinality and projected row width.
        let control = StorageReadControl::new(
            &MemoryBudget::new(64 * 1024),
            context.scans.runtime.cancellation,
        );
        Ok(Self {
            documents: context.documents,
            table,
            source,
            changes: if command_visible {
                context
                    .scans
                    .tables
                    .command_overlay_changes(table)?
                    .unwrap_or_default()
            } else {
                DocumentChanges::default()
            },
            page: BudgetedVec::new(control.memory()),
            control,
            serializable: SerializableScan::new(context.scans.tables.serializable_read(table)?),
            candidates: candidates.map(Vec::into_iter),
            after: None,
            position: 0,
            columns,
            projected,
            metadata,
            table_oid,
            origin: (qualifier.into(), table.into()),
        })
    }

    fn next_identity(&mut self) -> Result<Option<DocId>, SQLError> {
        self.control
            .check()
            .map_err(|error| storage_error("read WHERE candidates", &error))?;
        if let Some(candidates) = &mut self.candidates {
            for id in candidates {
                self.control
                    .check()
                    .map_err(|error| storage_error("read WHERE indexed candidates", &error))?;
                self.serializable.observe_row(id)?;
                let changed = self
                    .changes
                    .change_presence(id)
                    .map_err(|error| storage_error("read WHERE private identity", &error))?;
                let present = match changed {
                    Some(present) => present,
                    None => self
                        .source
                        .read_documents()
                        .contains_doc_id(id)
                        .map_err(|error| storage_error("read WHERE indexed identity", &error))?,
                };
                if present {
                    return Ok(Some(id));
                }
            }
            return Ok(None);
        }
        if self.position == self.page.len() {
            self.serializable.observe_relation()?;
            self.page = VisibleDocumentIds {
                source: self.source.read_documents().as_ref(),
                changes: &self.changes,
                control: &self.control,
            }
            .page(self.after, 128, &self.control, true)
            .map_err(|error| storage_error("read WHERE identity page", &error))?;
            self.after = self.page.last().copied().or(self.after);
            self.position = 0;
        }
        let id = self.page.get(self.position).copied();
        self.position += usize::from(id.is_some());
        Ok(id)
    }

    fn row(&self, id: DocId) -> Result<crate::PhysicalRow, SQLError> {
        let mut values = if self.projected {
            if self.columns.is_empty() {
                Vec::new()
            } else {
                let fields = self.columns.iter().map(String::as_str).collect::<Vec<_>>();
                self.documents
                    .document_fields(self.table, &[id], &fields)?
                    .remove(&id)
                    .ok_or_else(|| self.disappeared(id))?
            }
        } else {
            let document = self
                .documents
                .document(self.table, id)?
                .ok_or_else(|| self.disappeared(id))?;
            self.columns
                .iter()
                .map(|name| document.get(name).cloned().unwrap_or(Value::Null))
                .collect()
        };
        if values.len() != self.columns.len() {
            return Err(SQLError::Internal(
                "WHERE projection returned an invalid field count".into(),
            ));
        }
        values.extend(self.table_oid.clone());
        if self.metadata {
            values.push(uqa_sql::semantics::doc_id_value(id)?);
        }
        Ok(crate::PhysicalRow::from_values(values).with_lock_origin(
            crate::RowLockOrigin::from_shared(
                Arc::clone(&self.origin.0),
                Arc::clone(&self.origin.1),
                id,
            ),
        ))
    }

    fn disappeared(&self, id: DocId) -> SQLError {
        SQLError::Internal(format!(
            "WHERE scan: document {id} listed by table `{}` disappeared during the statement",
            self.table
        ))
    }
}

impl Iterator for ScalarRows<'_> {
    type Item = crate::ExecResult<crate::PhysicalRow>;
    fn next(&mut self) -> Option<Self::Item> {
        match self.next_identity() {
            Ok(Some(id)) => Some(self.row(id).map_err(Into::into)),
            Ok(None) => None,
            Err(error) => Some(Err(error.into())),
        }
    }
}
