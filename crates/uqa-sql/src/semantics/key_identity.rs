//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The physical document identity an integer primary-key value names.

use uqa_core::{DocId, Value};

/// A table whose single primary-key column is an integer names each row whose key lies in `0..KEY_IDENTITY_LIMIT` by the document identity equal to the key, so a key lookup reads that one identity. Its other rows, with negative keys or keys at or above the limit, take identities the table generates at or above the limit, where no key value names one; a key lookup reaches them through the key's index.
pub const KEY_IDENTITY_LIMIT: DocId = 1 << 62;

/// The document identity an integer primary-key value names, or `None` for a value no identity is named by.
#[must_use]
pub fn key_document_id(value: &Value) -> Option<DocId> {
    match value {
        Value::Int(key) => DocId::try_from(*key)
            .ok()
            .filter(|doc_id| *doc_id < KEY_IDENTITY_LIMIT),
        _ => None,
    }
}

/// Whether `doc_id` is an identity an integer primary-key value names.
#[must_use]
pub const fn is_key_document_id(doc_id: DocId) -> bool {
    doc_id < KEY_IDENTITY_LIMIT
}
