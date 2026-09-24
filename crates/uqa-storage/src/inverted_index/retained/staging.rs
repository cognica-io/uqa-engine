//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepare complete analyzed documents before changing the retained corpus.

use super::{
    copy_name, DocId, FieldName, IndexedFieldMetadata, MemoryInvertedIndex, StagedMemoryDocument,
    StorageBackendResult, StorageReadControl,
};
use crate::inverted_index::{analyze_index_field_budgeted, MemoryPosting, PostingKey};
use uqa_core::memory::{Budgeted, BudgetedMap, BudgetedVec, MemoryReservation, OwnedMap, OwnedSet};

struct Staging {
    fields: OwnedMap<FieldName, IndexedFieldMetadata>,
    terms: OwnedSet<PostingKey>,
    postings: BudgetedVec<(PostingKey, MemoryPosting)>,
    payload: MemoryReservation,
}

pub(super) fn stage(
    index: &MemoryInvertedIndex,
    doc_id: DocId,
    fields: &BudgetedMap<&str, &str>,
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<StagedMemoryDocument>> {
    let mut staged = Staging {
        fields: OwnedMap::new(),
        terms: OwnedSet::new(),
        postings: BudgetedVec::new(control.memory()),
        payload: control.memory().empty_reservation(),
    };
    for (field, text) in fields {
        staged.add_field(index, doc_id, field, text, control)?;
    }
    control.check()?;
    let (postings, memory) = staged.postings.into_parts();
    staged.payload.absorb(memory);
    Ok(Budgeted::new(
        StagedMemoryDocument {
            fields: staged.fields,
            terms: staged.terms,
            postings,
        },
        staged.payload,
    ))
}

impl Staging {
    fn add_field(
        &mut self,
        index: &MemoryInvertedIndex,
        doc_id: DocId,
        field: &str,
        text: &str,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        control.check()?;
        let revision = index.bindings.index_revision(field)?;
        let analyzed = analyze_index_field_budgeted(&revision, text, control.memory(), || {
            control
                .cancellation()
                .check()
                .map_err(|_| uqa_analysis::AnalysisError::Cancelled)
        })?;
        let metadata = IndexedFieldMetadata::new(&revision, &analyzed);
        let scratch_entries = analyzed.terms.allocated_bytes();
        let (analyzed, memory) = analyzed.into_parts();
        // The lease covers both the undrained analysis map and entries already moved into staging on every early return.
        self.payload.absorb(memory);
        self.payload
            .grow(OwnedMap::<FieldName, IndexedFieldMetadata>::entry_bytes())?;
        let name = copy_name(field, &mut self.payload, control)?;
        self.fields.insert(name, metadata);
        for (term, occurrences) in analyzed.terms {
            control.check()?;
            self.postings.reserve(1)?;
            self.payload.grow(OwnedSet::<PostingKey>::entry_bytes())?;
            let reverse_field = copy_name(field, &mut self.payload, control)?;
            let reverse_term = term.clone_budgeted(control.memory(), || control.check())?;
            let (reverse_term, memory) = reverse_term.into_parts();
            self.payload.absorb(memory);
            self.terms.insert((reverse_field, reverse_term));

            let name = copy_name(field, &mut self.payload, control)?;
            let mut positions = BudgetedVec::new(control.memory());
            positions.reserve(occurrences.len())?;
            for occurrence in &occurrences {
                control.check()?;
                positions.push(occurrence.position)?;
            }
            let (positions, memory) = positions.into_parts();
            self.payload.absorb(memory);
            self.postings.push((
                (name, term),
                MemoryPosting::new(doc_id, occurrences, positions),
            ))?;
        }
        drop(self.payload.split(scratch_entries));
        Ok(())
    }
}
