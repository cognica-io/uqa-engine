//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Values produced while selecting and applying a MERGE action.

use super::Document;

pub(in crate::sql::dml::merge) type MergeTargetIdentity = (String, uqa_core::DocId);

pub(in crate::sql::dml::merge) enum SelectedMergeAction {
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
