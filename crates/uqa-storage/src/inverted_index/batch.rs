//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Atomic batches stage affected documents with the original global counter context.

use super::{
    Arc, BTreeMap, BTreeSet, DocId, FieldName, MemoryIndexState, MemoryInvertedIndex,
    StorageBackendError, StorageBackendResult,
};

struct MemoryBatch {
    state: MemoryIndexState,
    documents: BTreeSet<DocId>,
    fields: BTreeSet<FieldName>,
}

impl MemoryBatch {
    fn new(
        source: &MemoryInvertedIndex,
        documents: &[(DocId, BTreeMap<FieldName, String>)],
    ) -> StorageBackendResult<Self> {
        let mut batch = Self {
            state: MemoryIndexState::default(),
            documents: documents.iter().map(|(id, _)| *id).collect(),
            fields: documents
                .iter()
                .flat_map(|(_, fields)| fields.keys().cloned())
                .collect(),
        };
        // Point replacements must see global totals even though this private projection retains only affected postings.
        let staged = &mut batch.state;
        staged.doc_count = source.state.doc_count;
        for &id in &batch.documents {
            let terms = source.state.doc_terms.get(&id);
            let fields = source.state.doc_fields.get(&id);
            if terms.is_some() != fields.is_some() {
                return Err(StorageBackendError::Other(format!(
                    "inverted-index document {id} has inconsistent reverse-index state"
                )));
            }
            if let (Some(terms), Some(fields)) = (terms, fields) {
                for key in terms {
                    let posting = source
                        .state
                        .index
                        .get(key)
                        .and_then(|postings| postings.get(&id))
                        .ok_or_else(|| {
                            StorageBackendError::Other(format!(
                                "inverted-index document {id} references a missing posting"
                            ))
                        })?;
                    staged
                        .index
                        .entry(key.clone())
                        .or_default()
                        .insert(id, posting.clone());
                }
                batch.fields.extend(fields.keys().cloned());
                staged.doc_terms.insert(id, terms.clone());
                staged.doc_fields.insert(id, fields.clone());
            }
        }
        for field in &batch.fields {
            if let Some(&length) = source.state.total_length.get(field) {
                staged.total_length.insert(field.clone(), length);
            }
            if let Some(&count) = source.state.field_doc_counts.get(field) {
                staged.field_doc_counts.insert(field.clone(), count);
            }
        }
        Ok(batch)
    }

    fn publish(self, target: &mut MemoryInvertedIndex) {
        // Validation and every fallible analysis/counter operation completed before this exclusive mutation.
        let staged = self.state;
        let target = Arc::make_mut(&mut target.state);
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
        for (key, postings) in staged.index {
            let target_postings = target.index.entry(key).or_default();
            for (id, posting) in postings {
                target_postings.insert(id, posting);
            }
        }
        for (id, terms) in staged.doc_terms {
            target.doc_terms.insert(id, terms);
        }
        for (id, fields) in staged.doc_fields {
            target.doc_fields.insert(id, fields);
        }
        for field in self.fields {
            match staged.total_length.get(&field) {
                Some(&length) => {
                    target.total_length.insert(field.clone(), length);
                }
                None => {
                    target.total_length.remove(&field);
                }
            }
            match staged.field_doc_counts.get(&field) {
                Some(&count) => {
                    target.field_doc_counts.insert(field, count);
                }
                None => {
                    target.field_doc_counts.remove(&field);
                }
            }
        }
        target.doc_count = staged.doc_count;
    }
}

impl MemoryInvertedIndex {
    pub(super) fn add_document_batch(
        &mut self,
        documents: Vec<(DocId, BTreeMap<FieldName, String>)>,
    ) -> StorageBackendResult<()> {
        self.add_document_batch_observed(documents, None)
    }

    pub(super) fn add_document_batch_observed(
        &mut self,
        documents: Vec<(DocId, BTreeMap<FieldName, String>)>,
        mut visit: Option<&mut super::InvertedIndexChangeVisitor<'_>>,
    ) -> StorageBackendResult<()> {
        if documents.is_empty() {
            return Ok(());
        }
        let mut batch = MemoryBatch::new(self, &documents)?;
        // Preserve ordered duplicate-ID replacements and intermediate overflow checks.
        for (id, fields) in documents {
            let staged = self.stage_document(id, fields)?;
            let plan = batch.state.plan_replacement(id, &staged.fields)?;
            if let Some(visit) = visit.as_mut() {
                batch.state.visit_replacement(*visit, id, &staged)?;
            }
            batch.state.apply_replacement(id, staged, plan)?;
        }
        batch.publish(self);
        Ok(())
    }
}

impl MemoryIndexState {
    fn visit_replacement(
        &self,
        visit: &mut super::InvertedIndexChangeVisitor<'_>,
        doc_id: DocId,
        replacement: &super::StagedMemoryDocument,
    ) -> StorageBackendResult<()> {
        let previous = self.doc_fields.get(&doc_id);
        for field in previous.into_iter().flat_map(BTreeMap::keys).chain(
            replacement
                .fields
                .keys()
                .filter(|field| !previous.is_some_and(|fields| fields.contains_key(*field))),
        ) {
            super::visit_field_replacement(
                visit,
                doc_id,
                field,
                previous
                    .and_then(|fields| fields.get(field))
                    .map(|metadata| metadata.length),
                replacement
                    .fields
                    .get(field)
                    .map(|metadata| metadata.length),
            )?;
        }
        for (field, term) in self
            .doc_terms
            .get(&doc_id)
            .into_iter()
            .flatten()
            .chain(&replacement.terms)
        {
            visit(super::InvertedIndexChange::Posting {
                doc_id,
                field,
                term,
            })?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
