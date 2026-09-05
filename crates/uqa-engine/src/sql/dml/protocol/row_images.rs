//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed old and new row images shared by mutation RETURNING and event paths.

use uqa_core::DocId;
use uqa_execution::OwnedPhysicalRow;
use uqa_storage::{document_store::Document, DocumentMetadata};

#[derive(Clone)]
pub(in crate::sql) struct RuleRowImage {
    pub(in crate::sql) old_storage_table: Option<String>,
    pub(in crate::sql) old_doc_id: Option<DocId>,
    pub(in crate::sql) old: Option<Document>,
    pub(in crate::sql) new_storage_table: Option<String>,
    pub(in crate::sql) new_doc_id: Option<DocId>,
    pub(in crate::sql) new: Option<Document>,
    pub(in crate::sql) context: Option<OwnedPhysicalRow>,
}

impl RuleRowImage {
    pub(in crate::sql) fn empty() -> Self {
        Self {
            old_storage_table: None,
            old_doc_id: None,
            old: None,
            new_storage_table: None,
            new_doc_id: None,
            new: None,
            context: None,
        }
    }

    pub(in crate::sql) fn supplement_documents(&mut self, supplemental: Self) {
        supplement_document(&mut self.old, supplemental.old);
        supplement_document(&mut self.new, supplemental.new);
    }
}

fn supplement_document(target: &mut Option<Document>, supplemental: Option<Document>) {
    let Some(supplemental) = supplemental else {
        return;
    };
    if let Some(target) = target {
        target.extend(supplemental);
    } else {
        *target = Some(supplemental);
    }
}

#[derive(Clone)]
pub(in crate::sql) struct MutationRowImage<'a> {
    pub storage_table: String,
    pub doc_id: DocId,
    pub document: &'a Document,
    pub metadata: DocumentMetadata,
}

#[derive(Clone)]
pub(in crate::sql) struct MutationRowImages<'a> {
    pub old: Option<MutationRowImage<'a>>,
    pub new: Option<MutationRowImage<'a>>,
}
