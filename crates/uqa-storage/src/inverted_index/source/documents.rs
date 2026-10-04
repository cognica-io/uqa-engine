//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Documents supplied together, read as a source.

use std::collections::BTreeMap;

use uqa_core::{DocId, FieldName};

use super::TextIndexSource;
use crate::backend::StorageBackendResult;

/// Documents supplied together, read in identity order. Of several documents with one identity, the last supplied is read, as a rebuild from a list replaced earlier copies.
pub struct TextIndexDocuments {
    documents: std::vec::IntoIter<(DocId, BTreeMap<FieldName, String>)>,
}

impl TextIndexDocuments {
    pub fn new(mut documents: Vec<(DocId, BTreeMap<FieldName, String>)>) -> Self {
        // A stable sort keeps the supplied order of one identity's copies, and reversing lets the deduplication keep the last of them.
        documents.sort_by_key(|(doc_id, _)| *doc_id);
        documents.reverse();
        documents.dedup_by_key(|(doc_id, _)| *doc_id);
        documents.reverse();
        Self {
            documents: documents.into_iter(),
        }
    }
}

impl From<Vec<(DocId, BTreeMap<FieldName, String>)>> for TextIndexDocuments {
    fn from(documents: Vec<(DocId, BTreeMap<FieldName, String>)>) -> Self {
        Self::new(documents)
    }
}

impl TextIndexSource for TextIndexDocuments {
    fn next_document(
        &mut self,
    ) -> StorageBackendResult<Option<(DocId, BTreeMap<FieldName, String>)>> {
        Ok(self.documents.next())
    }
}
