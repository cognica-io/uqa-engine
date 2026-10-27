//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Encode a pending view-trigger update for bounded retained ownership.

use super::{BTreeSet, OwnedPhysicalRow, SQLError, Value, ViewDmlTarget};
use crate::mutation::row_images::OwnedMutationImages;
use uqa_storage::document_store::Document;

pub(super) struct PendingViewUpdate {
    pub old: Vec<Value>,
    pub new: Vec<Value>,
    pub source_context: Option<OwnedPhysicalRow>,
    pub evaluation_row: OwnedPhysicalRow,
    pub evaluated_assignments: BTreeSet<usize>,
}

impl PendingViewUpdate {
    pub(super) fn into_image(
        self,
        target: &ViewDmlTarget,
    ) -> Result<OwnedMutationImages, SQLError> {
        if self.old.len() != target.columns.len() || self.new.len() != target.columns.len() {
            return Err(SQLError::Internal(
                "view rule row does not match its declared row type".into(),
            ));
        }
        Ok(OwnedMutationImages {
            old_storage_table: None,
            old_doc_id: None,
            old: Some(target.columns.iter().cloned().zip(self.old).collect()),
            new_storage_table: None,
            new_doc_id: None,
            new: Some(target.columns.iter().cloned().zip(self.new).collect()),
            context: self.source_context,
        })
    }
}

pub(super) fn view_values(
    target: &ViewDmlTarget,
    document: Option<Document>,
) -> Result<Vec<Value>, SQLError> {
    let mut document =
        document.ok_or_else(|| SQLError::Internal("pending view row lost its image".into()))?;
    target
        .columns
        .iter()
        .map(|column| {
            document.remove(column).ok_or_else(|| {
                SQLError::Internal(format!("pending view row lost column `{column}`"))
            })
        })
        .collect()
}
