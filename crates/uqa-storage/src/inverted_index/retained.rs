//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reconstructed text corpora reuse memory-index semantics with one retained payload allowance.

use super::{
    AnalyzerBindings, DocId, FieldName, IndexedFieldMetadata, InvertedIndex, MemoryIndexState,
    MemoryInvertedIndex, StagedMemoryDocument,
};
use crate::{
    read_control::StorageReadControl, ReadOnlySnapshot, StorageBackendError, StorageBackendResult,
};
use std::sync::Arc;
use uqa_core::memory::{Budgeted, BudgetedMap, BudgetedString, MemoryReservation};

mod charge;
mod input;
mod staging;

/// Build an admitted text corpus from borrowed selected fields or evaluated images. Analysis scratch, encoded terms, occurrence/position capacities and live corpus entries share the original allowance. Corpus map/set nodes use exact owned layouts admitted before publication; analyzer field bindings retain their own controlled containers. Allocator bookkeeping remains outside the payload charge. A rejected document leaves previously appended documents unchanged.
pub struct RetainedInvertedIndexBuilder {
    index: MemoryInvertedIndex,
    memory: MemoryReservation,
    control: StorageReadControl,
}

impl RetainedInvertedIndexBuilder {
    pub fn new(
        bindings: &AnalyzerBindings,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        let memory = control.memory().reserve(size_of::<MemoryIndexState>())?;
        Ok(Self {
            index: MemoryInvertedIndex::with_bindings(bindings.retained(control)?),
            memory,
            control: control.clone(),
        })
    }

    /// Capture selected compiled revisions and share the source's immutable default binding without cloning diagnostics or resolving resources again.
    pub fn from_revisions<'a>(
        default: super::AnalyzerDefault,
        fields: impl IntoIterator<
            Item = StorageBackendResult<(
                &'a str,
                Arc<uqa_analysis::CompiledAnalyzer>,
                Arc<uqa_analysis::CompiledAnalyzer>,
            )>,
        >,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        let bindings = AnalyzerBindings::from_retained_revisions(default, fields, control)?;
        Self::new(&bindings, control)
    }

    /// Populate retained field bindings through the enclosing input continuation. Both historical revisions remain shared; no analysis or resource resolution runs during field installation.
    pub async fn from_revisions_from<'a>(
        default: super::AnalyzerDefault,
        fields: impl IntoIterator<
            Item = StorageBackendResult<(
                &'a str,
                Arc<uqa_analysis::CompiledAnalyzer>,
                Arc<uqa_analysis::CompiledAnalyzer>,
            )>,
        >,
        control: &StorageReadControl,
        input: &mut uqa_core::image_io::AsyncImageInput<'_>,
    ) -> StorageBackendResult<Self> {
        let bindings =
            AnalyzerBindings::from_retained_revisions_from(default, fields, control, input).await?;
        Self::new(&bindings, control)
    }

    pub fn add_document<'a>(
        &mut self,
        doc_id: DocId,
        fields: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> StorageBackendResult<()> {
        self.control.check()?;
        if self.index.state.documents.contains_key(&doc_id) {
            return Err(StorageBackendError::Other(
                "retained text input repeats a document".into(),
            ));
        }
        // Preserve the ordinary map input's ordered, last-value field semantics without copying source strings.
        let mut borrowed = BudgetedMap::new(self.control.memory());
        for (field, text) in fields {
            self.control.check()?;
            borrowed.insert(field, text)?;
        }
        self.control.check()?;
        if borrowed.is_empty() {
            return Ok(());
        }
        let staged = staging::stage(&self.index, doc_id, &borrowed, &self.control)?;
        drop(borrowed);
        self.publish(doc_id, staged)
    }

    /// Append one standalone evaluated text image. Decoding and complete validation precede publication; errors leave prior documents intact. Stored occurrences, historical revisions and normalization lengths are preserved without invoking an analyzer or resource callback. The enclosing database image must separately supply and validate every referenced analyzer resource.
    pub fn add_image_document(
        &mut self,
        doc_id: DocId,
        input: &mut dyn std::io::Read,
    ) -> StorageBackendResult<()> {
        self.control.check()?;
        if self.index.state.documents.contains_key(&doc_id) {
            return Err(StorageBackendError::Other(
                "retained text input repeats a document".into(),
            ));
        }
        let staged = super::image::read(doc_id, input, &self.control)?;
        self.publish(doc_id, staged)
    }

    fn publish(
        &mut self,
        doc_id: DocId,
        staged: Budgeted<StagedMemoryDocument>,
    ) -> StorageBackendResult<()> {
        let control = self.control.clone();
        let publication = uqa_core::image_io::decode_image(
            &mut std::io::empty(),
            control.memory(),
            control.cancellation(),
            async |mut input| self.prepare_from(doc_id, staged, &mut input).await,
        )?;
        // Inline publication cannot yield or observe a new cancellation after the complete document has been admitted.
        crate::image_io::work::inline(self.publish_from(doc_id, publication, &mut ()))
    }

    /// Adopt evaluated postings into a writable native owner without analysis or resource resolution. Shared corpus roots retain their original leases; subsequent writes use the native mutation boundary and detach selected roots normally.
    pub fn finish_writable(mut self) -> StorageBackendResult<MemoryInvertedIndex> {
        self.control.check()?;
        self.index.image_memory = Some(
            self.control
                .memory()
                .reserve(size_of::<MemoryInvertedIndex>())?,
        );
        self.control.check()?;
        Arc::get_mut(&mut self.index.state)
            .expect("unpublished text builder owns its corpus")
            .image_memory = Some(self.memory);
        Ok(self.index)
    }

    pub fn finish(mut self) -> StorageBackendResult<ReadOnlySnapshot<dyn InvertedIndex>> {
        self.control.check()?;
        self.memory.grow(size_of::<MemoryInvertedIndex>())?;
        self.memory.grow(size_of::<MemoryReservation>())?;
        self.index.read_control = Some(self.control.clone());
        let memory = Arc::new(self.memory);
        self.index.state_memory = Some(Arc::clone(&memory));
        let index: Arc<dyn InvertedIndex> = Arc::new(self.index);
        ReadOnlySnapshot::with_shared_retention(index, memory)
            .with_inverted_read_control(&self.control)
    }
}

pub(super) fn copy_name(
    name: &str,
    memory: &mut MemoryReservation,
    control: &StorageReadControl,
) -> StorageBackendResult<String> {
    control.check()?;
    let mut copy = BudgetedString::new(control.memory());
    copy.reserve(name.len())?;
    for (index, character) in name.chars().enumerate() {
        if index % 1024 == 0 {
            control.check()?;
        }
        copy.push(character)?;
    }
    let (copy, reservation) = copy.into_parts();
    memory.absorb(reservation);
    Ok(copy)
}

#[cfg(test)]
pub(super) mod tests;
