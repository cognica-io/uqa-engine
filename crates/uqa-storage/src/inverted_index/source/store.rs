//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The text of a document store's indexed fields, read as a source a page of documents at a time.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use uqa_core::{CancellationToken, DocId, FieldName, Value};

use super::TextIndexSource;
use crate::backend::StorageBackendResult;
use crate::DocumentStore;

/// The documents a page holds at most.
const PAGE_DOCUMENTS: usize = 1024;
/// The text a page holds, after which it ends at the next document.
const PAGE_TEXT_BYTES: usize = 4 << 20;

/// The text of the indexed fields of a document store's documents, read a page at a time in identity order. A document holding no text in those fields is not read.
pub struct DocumentTextSource {
    store: Arc<dyn DocumentStore>,
    fields: Vec<FieldName>,
    /// The last document a page read.
    after: Option<DocId>,
    page: VecDeque<(DocId, BTreeMap<FieldName, String>)>,
    done: bool,
    cancellation: Option<CancellationToken>,
}

impl DocumentTextSource {
    /// The text of `fields` in the documents of `store`, which should be an immutable view of them, as a snapshot is.
    pub fn new(
        store: Arc<dyn DocumentStore>,
        fields: Vec<FieldName>,
        cancellation: Option<CancellationToken>,
    ) -> Self {
        Self {
            store,
            fields,
            after: None,
            page: VecDeque::new(),
            done: false,
            cancellation,
        }
    }

    fn check(&self) -> StorageBackendResult<()> {
        if let Some(cancellation) = &self.cancellation {
            cancellation.check()?;
        }
        Ok(())
    }

    fn read_page(&mut self) -> StorageBackendResult<()> {
        self.check()?;
        let ids = self.store.next_doc_ids(self.after, PAGE_DOCUMENTS)?;
        let Some(&last) = ids.last() else {
            self.done = true;
            return Ok(());
        };
        let Self {
            store,
            fields,
            page,
            cancellation,
            ..
        } = self;
        let names = fields.iter().map(String::as_str).collect::<Vec<_>>();
        let mut bytes = 0_usize;
        let mut ended = None;
        store.for_each_fields_multi_ref(&ids, &names, &mut |doc_id, values| {
            if cancellation
                .as_ref()
                .is_some_and(CancellationToken::is_cancelled)
            {
                ended = Some(doc_id);
                return false;
            }
            let mut text = BTreeMap::new();
            for (field, value) in fields.iter().zip(values) {
                if let Value::Str(value) = value {
                    bytes = bytes.saturating_add(value.len());
                    text.insert(field.clone(), value.clone());
                }
            }
            if !text.is_empty() {
                page.push_back((doc_id, text));
            }
            if bytes >= PAGE_TEXT_BYTES {
                ended = Some(doc_id);
                return false;
            }
            true
        })?;
        self.check()?;
        self.after = Some(ended.unwrap_or(last));
        Ok(())
    }
}

impl TextIndexSource for DocumentTextSource {
    fn next_document(
        &mut self,
    ) -> StorageBackendResult<Option<(DocId, BTreeMap<FieldName, String>)>> {
        while self.page.is_empty() && !self.done {
            self.read_page()?;
        }
        Ok(self.page.pop_front())
    }
}
