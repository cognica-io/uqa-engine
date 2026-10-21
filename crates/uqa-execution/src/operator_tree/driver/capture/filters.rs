//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Preserve index selection, statement support and relational predicate observations in bounded capture.

use super::{
    CapturedScoredEntries, DriverResult, OperatorTree, PhysicalRetrievalDriver, ScoredEntryCapture,
};
use crate::query::SelectedIds;
use uqa_core::{Predicate, ScoredEntry};
use uqa_sql::SQLError;

impl PhysicalRetrievalDriver<'_> {
    fn capture_indexed_ids(
        &self,
        field: &str,
        predicate: &Predicate,
        bytes: usize,
    ) -> DriverResult<Option<SelectedIds>> {
        crate::query::index_candidates::capture_predicate(
            self.context.indexes,
            self.table,
            field,
            predicate,
            true,
            bytes,
            &self.context.runtime.cancellation_token(),
        )
    }

    fn capture_ids_as_scores(
        &self,
        ids: &SelectedIds,
        bytes: usize,
    ) -> DriverResult<CapturedScoredEntries> {
        let mut output = ScoredEntryCapture::new(bytes, self.context.runtime.cancellation_token())?;
        for id in ids.cursor() {
            output.push(ScoredEntry {
                doc_id: id?,
                score: 0.0,
            })?;
        }
        output.finish()
    }

    pub(super) fn capture_filter(
        &self,
        field: &str,
        predicate: &Predicate,
        source: Option<&OperatorTree>,
        support: Option<&SelectedIds>,
        bytes: usize,
    ) -> DriverResult<CapturedScoredEntries> {
        self.require_column(field)?;
        if let Some(ids) = self.capture_indexed_ids(field, predicate, bytes / 3)? {
            let indexed = self.capture_ids_as_scores(&ids, bytes / 3)?;
            drop(ids);
            return match source {
                Some(source) => self.capture_tree(source, bytes / 3)?.merge_text_scores(
                    indexed,
                    true,
                    bytes / 3,
                    self.context.runtime.cancellation_token(),
                ),
                None => Ok(indexed),
            };
        }
        let cancellation = self.context.runtime.cancellation_token();
        let candidates = match source {
            Some(source) => {
                let entries = self.capture_tree(source, bytes / 2)?;
                let read = self.context.relations.serializable_read(self.table)?;
                SelectedIds::capture(
                    entries.map(|entry| {
                        let id = entry?.doc_id;
                        if let Some(read) = &read {
                            read.observe_row(id)?;
                        }
                        Ok(id)
                    }),
                    bytes / 2,
                    &cancellation,
                )?
            }
            None => {
                if let Some(read) = self.context.relations.serializable_read(self.table)? {
                    read.observe_scan()?;
                }
                match support {
                    Some(support) => support.clone(),
                    None => self.capture_relation_ids(bytes / 2)?,
                }
            }
        };
        self.context.relations.capture_document_filter(
            self.table,
            &candidates,
            field,
            predicate,
            bytes / 2,
            &cancellation,
        )
    }

    fn capture_relation_ids(&self, bytes: usize) -> DriverResult<SelectedIds> {
        let cancellation = self.context.runtime.cancellation_token();
        SelectedIds::capture_visit(bytes, &cancellation, |visit| {
            crate::mutation::candidate_ids::visit_identity_pages(
                &cancellation,
                |after, limit, control| {
                    self.context
                        .relations
                        .table_doc_id_page(self.table, after, limit, control)
                },
                visit,
            )
        })
    }

    pub(super) fn capture_index_scan(
        &self,
        index_name: &str,
        field: &str,
        predicate: &Predicate,
        bytes: usize,
    ) -> DriverResult<CapturedScoredEntries> {
        self.require_index_scan(index_name, field)?;
        let ids = self
            .capture_indexed_ids(field, predicate, bytes / 2)?
            .ok_or_else(|| {
                SQLError::Unsupported(format!(
                    "index {index_name:?} cannot evaluate predicate {predicate:?}"
                ))
            })?;
        self.capture_ids_as_scores(&ids, bytes / 2)
    }

    pub(super) fn capture_complement(
        &self,
        source: &OperatorTree,
        bytes: usize,
    ) -> DriverResult<CapturedScoredEntries> {
        if !self
            .context
            .relations
            .has_table(self.table)
            .map_err(|error| {
                super::super::operator_execution_error("resolve complement table", error)
            })?
        {
            return Err(SQLError::UnknownTable(self.table.to_owned()));
        }
        let mut included = self.capture_tree(source, bytes / 3)?.peekable();
        if let Some(read) = self.context.relations.serializable_read(self.table)? {
            read.observe_scan()?;
        }
        let ids = self.capture_relation_ids(bytes / 3)?;
        let mut output =
            ScoredEntryCapture::new(bytes / 3, self.context.runtime.cancellation_token())?;
        for id in ids.cursor() {
            let id = id?;
            let present = loop {
                match included.peek() {
                    Some(Ok(entry)) if entry.doc_id < id => {
                        included.next();
                    }
                    Some(Ok(entry)) => break entry.doc_id == id,
                    Some(Err(_)) => {
                        return Err(included.next().expect("peeked error").unwrap_err())
                    }
                    None => break false,
                }
            };
            if !present {
                output.push(ScoredEntry {
                    doc_id: id,
                    score: 0.0,
                })?;
            }
        }
        output.finish()
    }
}
