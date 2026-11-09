//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Opaque scoring parameters retain immutable catalog roots without copying unrelated payloads.

use uqa_core::memory::{Budgeted, MemoryBudget, MemoryError, RetainedMap, RetainedMapSnapshot};

use crate::{read_control::StorageReadControl, StorageBackendResult};

mod image;

/// Ordered, transactionally replaceable scoring metadata. Clones share an immutable root; updates copy only the affected tree path and never clone another entry's encoded payload.
#[derive(Clone)]
pub struct ScoringParameters {
    values: RetainedMap<String, Budgeted<String>>,
}

impl Default for ScoringParameters {
    fn default() -> Self {
        Self::new(&MemoryBudget::new(usize::MAX))
    }
}

impl ScoringParameters {
    /// Keep existing image payloads and reservations in place while selecting the allowance for future native replacements.
    pub fn into_mutation_budget(self, memory: &MemoryBudget) -> Self {
        Self {
            values: self.values.into_mutation_budget(memory),
        }
    }

    pub fn new(memory: &MemoryBudget) -> Self {
        Self {
            values: RetainedMap::new(memory),
        }
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn get(&self, name: &str) -> Option<&String> {
        self.values.get(name).map(|value| &**value)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &String)> {
        self.values.iter().map(|(name, value)| (name, &**value))
    }

    /// Failed path admission leaves both membership and its retained-payload total unchanged.
    pub fn insert(&mut self, name: String, encoded: String) -> Result<(), MemoryError> {
        let payload_bytes = name
            .capacity()
            .checked_add(encoded.capacity())
            .ok_or(MemoryError::SizeOverflow)?;
        let memory = self.values.budget().reserve(payload_bytes)?;
        self.values
            .insert(name, Budgeted::new(encoded, memory), payload_bytes)
    }

    pub fn remove(&mut self, name: &str) -> Result<bool, MemoryError> {
        self.values.remove(name)
    }

    pub fn clear(&mut self) {
        self.values.clear();
    }

    /// Build a replacement before publication, preserving the original root on failure.
    pub fn replace(
        &mut self,
        values: impl IntoIterator<Item = (String, String)>,
    ) -> Result<(), MemoryError> {
        let mut candidate = Self::new(self.values.budget());
        for (name, encoded) in values {
            candidate.insert(name, encoded)?;
        }
        *self = candidate;
        Ok(())
    }

    /// Admit the entire retained root independently, in constant work and without enumerating keys or copying payloads. Later reads borrow the selected entries in order.
    pub fn capture(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Budgeted<CapturedScoringParameters>> {
        control.check()?;
        let (values, memory) = self.values.capture(control.memory())?.into_parts();
        control.check()?;
        Ok(Budgeted::new(CapturedScoringParameters { values }, memory))
    }
}

/// One immutable scoring catalog generation. Its containing reservation owns the complete structural and encoded-payload retention, independently of other captures and later writes.
pub struct CapturedScoringParameters {
    values: RetainedMapSnapshot<String, Budgeted<String>>,
}

impl CapturedScoringParameters {
    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn get(&self, name: &str) -> Option<&String> {
        self.values.get(name).map(|value| &**value)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &String)> {
        self.values.iter().map(|(name, value)| (name, &**value))
    }
}

#[cfg(test)]
mod tests;
