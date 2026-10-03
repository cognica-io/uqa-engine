//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The AFTER row events a command queues as it stages its rows.

use uqa_core::DocId;
use uqa_storage::document_store::Document;

/// The AFTER row events of one command, in the order it staged its rows.
#[derive(Default)]
pub struct MutationEventQueue {
    after_rows: Vec<crate::mutation::triggers::AfterRowTriggerEvent>,
}

impl MutationEventQueue {
    pub fn after_rows(&self) -> &[crate::mutation::triggers::AfterRowTriggerEvent] {
        &self.after_rows
    }

    pub fn after_rows_mut(&mut self) -> &mut Vec<crate::mutation::triggers::AfterRowTriggerEvent> {
        &mut self.after_rows
    }

    pub fn append_after_rows(
        &mut self,
        events: Vec<crate::mutation::triggers::AfterRowTriggerEvent>,
    ) {
        self.after_rows.extend(events);
    }

    pub fn into_after_rows(self) -> Vec<crate::mutation::triggers::AfterRowTriggerEvent> {
        self.after_rows
    }
}

pub struct ReferentialRewritePreparation<'a> {
    /// The table of the foreign key whose action rewrites the row.
    pub constraint_table: &'a str,
    pub table: &'a str,
    pub doc_id: DocId,
    pub old_document: Document,
    pub proposed_document: Document,
    pub updated_columns: Vec<String>,
}
