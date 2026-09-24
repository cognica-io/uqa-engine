//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Atomic batches stage affected documents with the original global counter context.

use super::{
    Arc, BTreeMap, BTreeSet, DocId, FieldName, MemoryIndexState, MemoryInvertedIndex, OwnedMap,
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
            if let Some(document) = source.state.document(id)? {
                for key in &document.terms {
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
                    staged.insert_posting(id, key.clone(), posting.clone());
                }
                batch.fields.extend(document.fields.keys().cloned());
                staged.insert_document_metadata(
                    id,
                    document.fields.clone(),
                    document.terms.clone(),
                );
            }
        }
        for field in &batch.fields {
            if let Some(&counters) = source.state.field_counters.get(field) {
                super::footprint::set_counter(
                    &mut staged.field_counters,
                    field.clone(),
                    Some(counters),
                    &mut staged.retention,
                );
            }
        }
        Ok(batch)
    }

    fn publish(self, target: &mut MemoryInvertedIndex) {
        // Validation and every fallible analysis/counter operation completed before this exclusive mutation.
        let staged = self.state;
        let target = Arc::make_mut(&mut target.state);
        for id in self.documents {
            if let Some(document) = target.take_document_metadata(id) {
                for key in &document.terms {
                    target
                        .remove_posting(id, key)
                        .expect("validated batch posting");
                }
            }
        }
        for (key, postings) in staged.index {
            target.insert_postings(key, postings);
        }
        for (id, document) in staged.documents {
            target.insert_document_metadata(id, document.fields, document.terms);
        }
        for field in self.fields {
            let counters = staged.field_counters.get(&field).copied();
            super::footprint::set_counter(
                &mut target.field_counters,
                field,
                counters,
                &mut target.retention,
            );
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
        let previous = self.documents.get(&doc_id).map(|document| &document.fields);
        for field in previous.into_iter().flat_map(OwnedMap::keys).chain(
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
            .documents
            .get(&doc_id)
            .into_iter()
            .flat_map(|document| &document.terms)
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
