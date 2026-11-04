//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Opaque scoring parameters retain immutable catalog roots without copying unrelated payloads.

use uqa_core::memory::{Budgeted, MemoryBudget, MemoryError, RetainedMap, RetainedMapSnapshot};

use crate::{read_control::StorageReadControl, StorageBackendResult};

/// Ordered, transactionally replaceable scoring metadata. Clones share an immutable root; updates copy only the affected tree path and never clone another entry's encoded payload.
#[derive(Clone)]
pub struct ScoringParameters {
    values: RetainedMap<String, String>,
}

impl Default for ScoringParameters {
    fn default() -> Self {
        Self::new(&MemoryBudget::new(usize::MAX))
    }
}

impl ScoringParameters {
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
        self.values.get(name)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &String)> {
        self.values.iter()
    }

    /// Failed path admission leaves both membership and its retained-payload total unchanged.
    pub fn insert(&mut self, name: String, encoded: String) -> Result<(), MemoryError> {
        let payload_bytes = name
            .capacity()
            .checked_add(encoded.capacity())
            .ok_or(MemoryError::SizeOverflow)?;
        self.values.insert(name, encoded, payload_bytes)
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
    values: RetainedMapSnapshot<String, String>,
}

impl CapturedScoringParameters {
    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn get(&self, name: &str) -> Option<&String> {
        self.values.get(name)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &String)> {
        self.values.iter()
    }
}

#[cfg(test)]
mod tests;
