//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native counter planning and publication share inline and retained work boundaries.

use super::{
    counter_error, footprint, DocId, FieldName, IndexedFieldMetadata, MemoryFieldCounters,
    MemoryIndexState, MemoryReplacementPlan, StagedMemoryDocument, StorageBackendError,
    StorageBackendResult,
};
use crate::image_io::work::{inline, Work};
use std::future::{ready, Future};
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{MemoryReservation, OwnedMap},
};

pub(super) trait Names {
    fn checkpoint(&mut self) -> impl Future<Output = StorageBackendResult<()>> + Send;
    fn copy(
        &mut self,
        name: &FieldName,
    ) -> impl Future<Output = StorageBackendResult<FieldName>> + Send;
}

struct InlineNames<F>(F);
impl<F: FnMut(&FieldName) -> StorageBackendResult<FieldName>> Names for InlineNames<F> {
    fn checkpoint(&mut self) -> impl Future<Output = StorageBackendResult<()>> + Send {
        ready(Ok(()))
    }
    fn copy(
        &mut self,
        name: &FieldName,
    ) -> impl Future<Output = StorageBackendResult<FieldName>> + Send {
        ready((self.0)(name))
    }
}

pub(super) struct AdmittedNames<'a, 'b> {
    pub(super) input: &'a mut AsyncImageInput<'b>,
    pub(super) memory: &'a mut MemoryReservation,
}

impl Names for AdmittedNames<'_, '_> {
    async fn checkpoint(&mut self) -> StorageBackendResult<()> {
        Ok(self.input.checkpoint().await?)
    }
    async fn copy(&mut self, name: &FieldName) -> StorageBackendResult<FieldName> {
        let (name, memory) = self.input.copy_text(name).await?.into_parts();
        self.memory.absorb(memory);
        Ok(name)
    }
}

impl MemoryIndexState {
    pub(super) fn plan_replacement(
        &self,
        doc_id: DocId,
        new_fields: &OwnedMap<FieldName, IndexedFieldMetadata>,
    ) -> StorageBackendResult<MemoryReplacementPlan> {
        self.plan_replacement_with_names(doc_id, new_fields, |field| Ok(field.clone()))
    }

    pub(super) fn plan_replacement_with_names(
        &self,
        doc_id: DocId,
        new_fields: &OwnedMap<FieldName, IndexedFieldMetadata>,
        copy_name: impl FnMut(&FieldName) -> StorageBackendResult<FieldName>,
    ) -> StorageBackendResult<MemoryReplacementPlan> {
        inline(self.plan_replacement_from(doc_id, new_fields, InlineNames(copy_name)))
    }

    pub(super) async fn plan_replacement_from(
        &self,
        doc_id: DocId,
        new_fields: &OwnedMap<FieldName, IndexedFieldMetadata>,
        mut names: impl Names,
    ) -> StorageBackendResult<MemoryReplacementPlan> {
        let previous = self.document(doc_id)?;
        let old_terms = previous
            .map(|document| document.terms.clone())
            .unwrap_or_default();
        let empty_fields = OwnedMap::new();
        let old_fields = previous.map_or(&empty_fields, |document| &document.fields);
        let next_doc_count = self
            .doc_count
            .checked_sub(u64::from(previous.is_some()))
            .ok_or_else(|| counter_error("document count"))?
            .checked_add(u64::from(!new_fields.is_empty()))
            .ok_or_else(|| counter_error("document count"))?;
        for key in &old_terms {
            names.checkpoint().await?;
            if !self
                .index
                .get(key)
                .is_some_and(|postings| postings.contains_key(&doc_id))
            {
                return Err(StorageBackendError::Other(format!(
                    "inverted-index document {doc_id} references a missing posting"
                )));
            }
        }

        // Merge the two already ordered field views without allocating a temporary set.
        let mut old_keys = old_fields.keys().peekable();
        let mut new_keys = new_fields.keys().peekable();
        let affected_fields = std::iter::from_fn(|| match (old_keys.peek(), new_keys.peek()) {
            (Some(old), Some(new)) => match old.cmp(new) {
                std::cmp::Ordering::Less => old_keys.next(),
                std::cmp::Ordering::Greater => new_keys.next(),
                std::cmp::Ordering::Equal => {
                    old_keys.next();
                    new_keys.next()
                }
            },
            (Some(_), None) => old_keys.next(),
            (None, Some(_)) => new_keys.next(),
            (None, None) => None,
        });
        let mut field_counters = OwnedMap::new();
        for field in affected_fields {
            names.checkpoint().await?;
            let old_length = old_fields.get(field).map_or(0, |metadata| metadata.length);
            let new_length = new_fields.get(field).map_or(0, |metadata| metadata.length);
            let total = self
                .field_counters
                .get(field)
                .map_or(0, |counters| counters.total)
                .checked_sub(old_length)
                .ok_or_else(|| counter_error("total field length"))?
                .checked_add(new_length)
                .ok_or_else(|| counter_error("total field length"))?;
            let field_docs = self
                .field_counters
                .get(field)
                .map_or(0, |counters| counters.docs)
                .checked_sub(u64::from(old_fields.contains_key(field)))
                .ok_or_else(|| counter_error("field document count"))?
                .checked_add(u64::from(new_fields.contains_key(field)))
                .ok_or_else(|| counter_error("field document count"))?;
            field_counters.insert(
                names.copy(field).await?,
                MemoryFieldCounters {
                    total,
                    docs: field_docs,
                },
            );
        }
        Ok(MemoryReplacementPlan {
            old_terms,
            next_doc_count,
            field_counters,
        })
    }

    pub(super) fn apply_replacement(
        &mut self,
        doc_id: DocId,
        staged: StagedMemoryDocument,
        plan: MemoryReplacementPlan,
    ) -> StorageBackendResult<()> {
        inline(self.apply_replacement_from(doc_id, staged, plan, &mut ()))
    }

    pub(super) async fn apply_replacement_from(
        &mut self,
        doc_id: DocId,
        staged: StagedMemoryDocument,
        plan: MemoryReplacementPlan,
        work: &mut impl Work,
    ) -> StorageBackendResult<()> {
        for key in plan.old_terms {
            work.checkpoint().await?;
            self.remove_posting(doc_id, &key)?;
        }
        self.remove_document_metadata(doc_id);
        for (field, counters) in plan.field_counters {
            work.checkpoint().await?;
            footprint::set_counter(
                &mut self.field_counters,
                field,
                (counters.docs != 0).then_some(counters),
                &mut self.retention,
            );
        }
        for (key, entry) in staged.postings {
            work.checkpoint().await?;
            self.insert_posting(doc_id, key, entry);
        }
        self.doc_count = plan.next_doc_count;
        if !staged.fields.is_empty() {
            self.insert_document_metadata_from(doc_id, staged.fields, staged.terms, work)
                .await?;
        }
        Ok(())
    }
}
