//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Column constraint mutation, key and foreign-key metadata, and identifier allocation.

use super::{
    table_not_found, DocId, Engine, RelationIdentity, SQLError, StorageBackendError,
    StorageBackendResult, TableState,
};

pub(crate) use uqa_storage::document_store::identifiers::legacy_document_id_metadata_key as table_next_id_metadata_key;

mod engine;
