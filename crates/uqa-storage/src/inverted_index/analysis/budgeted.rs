//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Index analysis retains term keys and occurrence buffers under the source reader's allowance.

use super::{
    project_index_tokens, AnalysisError, AnalyzedField, BTreeMap, CompiledAnalyzer,
    IndexedFieldMetadata, StorageBackendError, StorageBackendResult, TokenOccurrence, TokenTerm,
    TokenTermKey,
};
use std::collections::btree_map::Entry;
use uqa_core::memory::{Budgeted, BudgetedVec, MemoryBudget, MemoryError, MemoryReservation};

/// Analyze a complete field and reserve its encoded keys, occurrence capacities and live map entries before allocation. Analysis scratch shares the same allowance and cancellation callback. Opaque map-node slack and allocator bookkeeping are outside the payload charge; the returned reservation follows the decoded field until its owner releases it.
pub fn analyze_index_field_budgeted(
    analyzer: &CompiledAnalyzer,
    text: &str,
    memory: &MemoryBudget,
    mut poll: impl FnMut() -> Result<(), AnalysisError>,
) -> StorageBackendResult<Budgeted<AnalyzedField>> {
    (|| {
        let stream = analyzer.analyze_tokens_budgeted(text, memory, &mut poll)?;
        let mut terms = Terms::new(memory);
        let metadata =
            project_index_tokens(analyzer, &stream, &mut poll, |term, occurrence, poll| {
                terms.push(term, occurrence, poll)
            })?;
        terms.finish(metadata, &mut poll)
    })()
    .map_err(|error| match error {
        StorageBackendError::Analysis(AnalysisError::Memory(memory)) => {
            StorageBackendError::Memory(memory)
        }
        StorageBackendError::Analysis(AnalysisError::Cancelled) => {
            StorageBackendError::Cancelled(uqa_core::QueryCancelled)
        }
        other => other,
    })
}

struct Terms {
    // Destroy every encoded key before releasing the aggregate key and entry reservations.
    values: BTreeMap<TokenTermKey, BudgetedVec<TokenOccurrence>>,
    keys: MemoryReservation,
    entries: MemoryReservation,
}

impl Terms {
    fn new(memory: &MemoryBudget) -> Self {
        Self {
            values: BTreeMap::new(),
            keys: memory.empty_reservation(),
            entries: memory.empty_reservation(),
        }
    }

    fn push(
        &mut self,
        term: &TokenTerm,
        occurrence: TokenOccurrence,
        poll: &mut impl FnMut() -> Result<(), AnalysisError>,
    ) -> StorageBackendResult<()> {
        let key = TokenTermKey::from_term_budgeted(term, self.keys.budget(), poll)?;
        let (key, key_memory) = key.into_parts();
        match self.values.entry(key) {
            Entry::Occupied(mut entry) => entry.get_mut().push(occurrence)?,
            Entry::Vacant(entry) => {
                let entry_memory = self
                    .entries
                    .budget()
                    .reserve(size_of::<(TokenTermKey, BudgetedVec<TokenOccurrence>)>())?;
                let mut occurrences = BudgetedVec::new(self.keys.budget());
                occurrences.push(occurrence)?;
                entry.insert(occurrences);
                self.keys.absorb(key_memory);
                self.entries.absorb(entry_memory);
            }
        }
        Ok(())
    }

    fn finish(
        self,
        metadata: IndexedFieldMetadata,
        poll: &mut impl FnMut() -> Result<(), AnalysisError>,
    ) -> StorageBackendResult<Budgeted<AnalyzedField>> {
        let bytes = self
            .values
            .len()
            .checked_mul(size_of::<(TokenTermKey, Vec<TokenOccurrence>)>())
            .ok_or(MemoryError::SizeOverflow)?;
        let output_memory = self.keys.budget().reserve(bytes)?;
        // Both map layouts remain charged during conversion; keys and occurrence buffers move without copying. This tuple keeps their reservations alive if publication is cancelled.
        let mut pending = (
            AnalyzedField {
                length: metadata.length,
                terms: BTreeMap::new(),
                final_offsets: metadata.final_offsets,
                final_position_increment: metadata.final_position_increment,
            },
            output_memory,
            self.keys,
            self.entries,
        );
        for (key, occurrences) in self.values {
            poll()?;
            let (occurrences, memory) = occurrences.into_parts();
            pending.0.terms.insert(key, occurrences);
            pending.1.absorb(memory);
        }
        poll()?;
        let (field, mut memory, keys, scratch_entries) = pending;
        drop(scratch_entries);
        memory.absorb(keys);
        Ok(Budgeted::new(field, memory))
    }
}

#[cfg(test)]
mod tests;
