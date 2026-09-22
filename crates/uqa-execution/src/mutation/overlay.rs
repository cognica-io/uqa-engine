//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Statement-local row overlays and exact index state.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use uqa_core::DocId;
use uqa_storage::document_store::{Document, DocumentMetadata, RetainedDocumentFields};
use uqa_storage::{read_control::StorageReadControl, StorageBackendResult};

#[derive(Clone)]
pub struct CommandStoredDocument {
    pub fields: RetainedDocumentFields,
    pub metadata: DocumentMetadata,
}

impl CommandStoredDocument {
    pub fn new(
        fields: Arc<Document>,
        metadata: DocumentMetadata,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        Ok(Self {
            fields: RetainedDocumentFields::new(fields, control)?,
            metadata,
        })
    }
}

#[derive(Clone, Default)]
pub struct CommandMutationOverlay {
    pub documents: BTreeMap<String, BTreeMap<DocId, Option<CommandStoredDocument>>>,
    pub exact_indexes: BTreeMap<String, BTreeMap<Vec<String>, CommandExactIndex>>,
}

#[derive(Clone, Default)]
pub struct CommandExactIndex {
    pub doc_ids_by_key: BTreeMap<Vec<u8>, BTreeSet<DocId>>,
}
