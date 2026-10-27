//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Values produced while selecting and applying a MERGE action.

use uqa_storage::document_store::Document;

pub(super) type MergeTargetIdentity = (String, uqa_core::DocId);

pub(super) enum SelectedMergeAction {
    Nothing,
    Update {
        doc_id: uqa_core::DocId,
        old_document: Document,
        new_document: Document,
        updated_columns: Vec<String>,
    },
    Delete {
        doc_id: uqa_core::DocId,
    },
    Insert {
        document: Document,
    },
}
