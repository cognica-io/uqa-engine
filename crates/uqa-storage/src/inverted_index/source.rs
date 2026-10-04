//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The documents a source rebuild of a text index reads, a document at a time, so that a source need not hold its documents together.

use std::collections::BTreeMap;

use uqa_core::{DocId, FieldName};

use crate::backend::StorageBackendResult;

mod documents;
mod store;
pub use documents::TextIndexDocuments;
pub use store::DocumentTextSource;

/// The documents a source rebuild of a text index reads: each document once, in strictly ascending identity order, with the text of its indexed fields.
pub trait TextIndexSource {
    /// The next document, or `None` after the last.
    fn next_document(
        &mut self,
    ) -> StorageBackendResult<Option<(DocId, BTreeMap<FieldName, String>)>>;
}

#[cfg(test)]
mod tests;
