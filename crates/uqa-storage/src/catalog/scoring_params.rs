//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Opaque scoring parameters retain immutable catalog roots without copying unrelated payloads.

use uqa_core::memory::{
    Budgeted, BudgetedSharedMap, BudgetedSharedMapSnapshot, MemoryBudget, MemoryError,
};

use crate::{read_control::StorageReadControl, StorageBackendResult};

struct Parameter {
    encoded: String,
    payload_bytes: usize,
}

/// Ordered, transactionally replaceable scoring metadata. Clones share an immutable root; updates copy only the affected tree path and never clone another entry's encoded payload.
#[derive(Clone)]
pub struct ScoringParameters {
    values: BudgetedSharedMap<String, Parameter>,
    payload_bytes: usize,
}

impl Default for ScoringParameters {
    fn default() -> Self {
        Self::new(&MemoryBudget::new(usize::MAX))
    }
}

impl ScoringParameters {
    pub fn new(memory: &MemoryBudget) -> Self {
        Self {
            values: BudgetedSharedMap::new(memory),
            payload_bytes: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn get(&self, name: &str) -> Option<&String> {
        self.values.get(name).map(|value| &value.encoded)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &String)> {
        self.values
            .iter()
            .map(|(name, value)| (name, &value.encoded))
    }

    /// Failed path admission leaves both membership and its retained-payload total unchanged.
    pub fn insert(&mut self, name: String, encoded: String) -> Result<(), MemoryError> {
        let payload_bytes = name
            .capacity()
            .checked_add(encoded.capacity())
            .ok_or(MemoryError::SizeOverflow)?;
        let previous = self
            .values
            .get(name.as_str())
            .map_or(0, |v| v.payload_bytes);
        let total = self
            .payload_bytes
            .checked_sub(previous)
            .and_then(|bytes| bytes.checked_add(payload_bytes))
            .ok_or(MemoryError::SizeOverflow)?;
        self.values.try_insert(
            name,
            Parameter {
                encoded,
                payload_bytes,
            },
        )?;
        self.payload_bytes = total;
        Ok(())
    }

    pub fn remove(&mut self, name: &str) -> Result<bool, MemoryError> {
        let Some(previous) = self.values.get(name) else {
            return Ok(false);
        };
        let total = self.payload_bytes - previous.payload_bytes;
        self.values = self.values.with_remove(name)?;
        self.payload_bytes = total;
        Ok(true)
    }

    pub fn clear(&mut self) {
        self.values = BudgetedSharedMap::new(self.values.budget());
        self.payload_bytes = 0;
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
        let values = self.values.snapshot();
        let bytes = values
            .structural_bytes()?
            .checked_add(self.payload_bytes)
            .ok_or(MemoryError::SizeOverflow)?;
        let memory = control.memory().reserve(bytes)?;
        control.check()?;
        Ok(Budgeted::new(CapturedScoringParameters { values }, memory))
    }
}

/// One immutable scoring catalog generation. Its containing reservation owns the complete structural and encoded-payload retention, independently of other captures and later writes.
pub struct CapturedScoringParameters {
    values: BudgetedSharedMapSnapshot<String, Parameter>,
}

impl CapturedScoringParameters {
    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn get(&self, name: &str) -> Option<&String> {
        self.values.get(name).map(|value| &value.encoded)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &String)> {
        self.values
            .iter()
            .map(|(name, value)| (name, &value.encoded))
    }
}

#[cfg(test)]
mod tests;
