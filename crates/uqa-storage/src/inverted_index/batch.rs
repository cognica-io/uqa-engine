//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Atomic batches stage affected documents with the original global counter context.

use super::{
    BTreeMap, BTreeSet, DocId, FieldName, InvertedIndex, MemoryInvertedIndex, StorageBackendError,
    StorageBackendResult,
};

struct MemoryBatch {
    index: MemoryInvertedIndex,
    documents: BTreeSet<DocId>,
    fields: BTreeSet<FieldName>,
}

impl MemoryBatch {
    fn new(
        source: &MemoryInvertedIndex,
        documents: &[(DocId, BTreeMap<FieldName, String>)],
    ) -> StorageBackendResult<Self> {
        let mut batch = Self {
            index: MemoryInvertedIndex::with_bindings(source.bindings.clone()),
            documents: documents.iter().map(|(id, _)| *id).collect(),
            fields: documents
                .iter()
                .flat_map(|(_, fields)| fields.keys().cloned())
                .collect(),
        };
        // Point replacements must see global totals even though this private projection retains only affected postings.
        batch.index.doc_count = source.doc_count;
        for &id in &batch.documents {
            let terms = source.doc_terms.get(&id);
            let fields = source.doc_fields.get(&id);
            if terms.is_some() != fields.is_some() {
                return Err(StorageBackendError::Other(format!(
                    "inverted-index document {id} has inconsistent reverse-index state"
                )));
            }
            if let (Some(terms), Some(fields)) = (terms, fields) {
                for key in terms {
                    let posting = source
                        .index
                        .get(key)
                        .and_then(|postings| postings.get(&id))
                        .ok_or_else(|| {
                            StorageBackendError::Other(format!(
                                "inverted-index document {id} references a missing posting"
                            ))
                        })?;
                    batch
                        .index
                        .index
                        .entry(key.clone())
                        .or_default()
                        .insert(id, posting.clone());
                }
                batch.fields.extend(fields.keys().cloned());
                batch.index.doc_terms.insert(id, terms.clone());
                batch.index.doc_fields.insert(id, fields.clone());
            }
        }
        for field in &batch.fields {
            if let Some(&length) = source.total_length.get(field) {
                batch.index.total_length.insert(field.clone(), length);
            }
            if let Some(&count) = source.field_doc_counts.get(field) {
                batch.index.field_doc_counts.insert(field.clone(), count);
            }
        }
        Ok(batch)
    }

    fn publish(self, target: &mut MemoryInvertedIndex) {
        // Validation and every fallible analysis/counter operation completed before this exclusive mutation.
        for id in self.documents {
            if let Some(terms) = target.doc_terms.remove(&id) {
                for key in terms {
                    let postings = target.index.get_mut(&key).expect("validated batch posting");
                    postings.remove(&id);
                    if postings.is_empty() {
                        target.index.remove(&key);
                    }
                }
            }
            target.doc_fields.remove(&id);
        }
        for (key, postings) in self.index.index {
            let target_postings = target.index.entry(key).or_default();
            for (id, posting) in postings {
                target_postings.insert(id, posting);
            }
        }
        for (id, terms) in self.index.doc_terms {
            target.doc_terms.insert(id, terms);
        }
        for (id, fields) in self.index.doc_fields {
            target.doc_fields.insert(id, fields);
        }
        for field in self.fields {
            match self.index.total_length.get(&field) {
                Some(&length) => {
                    target.total_length.insert(field.clone(), length);
                }
                None => {
                    target.total_length.remove(&field);
                }
            }
            match self.index.field_doc_counts.get(&field) {
                Some(&count) => {
                    target.field_doc_counts.insert(field, count);
                }
                None => {
                    target.field_doc_counts.remove(&field);
                }
            }
        }
        target.doc_count = self.index.doc_count;
    }
}

impl MemoryInvertedIndex {
    pub(super) fn add_document_batch(
        &mut self,
        documents: Vec<(DocId, BTreeMap<FieldName, String>)>,
    ) -> StorageBackendResult<()> {
        if documents.is_empty() {
            return Ok(());
        }
        let mut batch = MemoryBatch::new(self, &documents)?;
        // Preserve ordered duplicate-ID replacements and intermediate overflow checks.
        for (id, fields) in documents {
            batch.index.add_document(id, fields)?;
        }
        batch.publish(self);
        Ok(())
    }
}

#[cfg(test)]
mod tests;
