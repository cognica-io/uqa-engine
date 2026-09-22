//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reconstructed text corpora reuse memory-index semantics with one retained payload allowance.

use super::{
    AnalyzerBindings, BTreeMap, DocId, FieldName, IndexedFieldMetadata, InvertedIndex,
    MemoryFieldCounters, MemoryIndexState, MemoryInvertedIndex, MemoryReplacementPlan,
    StagedMemoryDocument,
};
use crate::{
    read_control::StorageReadControl, ReadOnlySnapshot, StorageBackendError, StorageBackendResult,
};
use std::sync::Arc;
use uqa_core::memory::{Budgeted, BudgetedString, MemoryError, MemoryReservation};

mod charge;
mod staging;

/// Build an immutable text index from borrowed selected fields. Analysis scratch, encoded terms, occurrence/position capacities and live corpus entries share the original allowance. Analyzer bindings and opaque map-node/allocator bookkeeping are outside this payload charge. A rejected document leaves previously appended documents unchanged.
pub struct RetainedInvertedIndexBuilder {
    index: MemoryInvertedIndex,
    memory: MemoryReservation,
    control: StorageReadControl,
}

impl RetainedInvertedIndexBuilder {
    pub fn new(
        bindings: AnalyzerBindings,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        let memory = control.memory().reserve(size_of::<MemoryIndexState>())?;
        Ok(Self {
            index: MemoryInvertedIndex::with_bindings(bindings),
            memory,
            control: control.clone(),
        })
    }

    pub fn add_document<'a>(
        &mut self,
        doc_id: DocId,
        fields: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> StorageBackendResult<()> {
        self.control.check()?;
        if self.index.state.doc_fields.contains_key(&doc_id) {
            return Err(StorageBackendError::Other(
                "retained text input repeats a document".into(),
            ));
        }
        // Preserve the ordinary map input's ordered, last-value field semantics without copying source strings.
        let mut borrowed = (BTreeMap::new(), self.control.memory().empty_reservation());
        for (field, text) in fields {
            self.control.check()?;
            if !borrowed.0.contains_key(field) {
                borrowed.1.grow(size_of::<(&str, &str)>())?;
            }
            borrowed.0.insert(field, text);
        }
        if borrowed.0.is_empty() {
            return Ok(());
        }
        let staged = staging::stage(&self.index, doc_id, &borrowed.0, &self.control)?;
        drop(borrowed);
        let plan = self.plan(doc_id, &staged.fields)?;
        let charge = charge::new_document(&self.index.state, &staged, &plan, &self.control)?;
        let new_entries = self.control.memory().reserve(charge.new_entries)?;
        self.control.check()?;
        // Every fallible allocation and validation precedes mutation. The existing application has no old postings to remove for this unique identity.
        let (staged, memory) = staged.into_parts();
        let (plan, plan_memory) = plan.into_parts();
        let mut pending = (staged, plan, memory);
        pending.2.absorb(plan_memory);
        pending.2.absorb(new_entries);
        let result = Arc::get_mut(&mut self.index.state)
            .expect("unpublished text builder owns its corpus")
            .apply_replacement(doc_id, pending.0, pending.1);
        self.memory.absorb(pending.2.split(charge.retained));
        // Application consumed the staging containers and discarded duplicate global keys before their remaining reservations are released.
        result
    }

    fn plan(
        &self,
        doc_id: DocId,
        fields: &BTreeMap<FieldName, IndexedFieldMetadata>,
    ) -> StorageBackendResult<Budgeted<MemoryReplacementPlan>> {
        let mut memory = self.control.memory().reserve(
            fields
                .len()
                .checked_mul(size_of::<(FieldName, MemoryFieldCounters)>())
                .ok_or(MemoryError::SizeOverflow)?,
        )?;
        let affected = self.control.memory().reserve(
            fields
                .len()
                .checked_mul(size_of::<&FieldName>())
                .ok_or(MemoryError::SizeOverflow)?,
        )?;
        let plan = self
            .index
            .state
            .plan_replacement_with_names(doc_id, fields, |name| {
                copy_name(name, &mut memory, &self.control)
            })?;
        drop(affected);
        Ok(Budgeted::new(plan, memory))
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

fn copy_name(
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
