//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered canonical input is checked against live graph identities one vector at a time.

use super::{types::active_key, HNSWIndex};
use crate::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};
use uqa_core::DocId;

/// Check a complete ordered canonical stream without retaining another corpus or identity directory.
pub struct HNSWCanonicalValidator<'a> {
    index: &'a HNSWIndex,
    control: &'a StorageReadControl,
    after: Option<u128>,
    failed: bool,
}

impl<'a> HNSWCanonicalValidator<'a> {
    pub fn new(index: &'a HNSWIndex, control: &'a StorageReadControl) -> Self {
        Self {
            index,
            control,
            after: None,
            failed: false,
        }
    }

    pub fn push(
        &mut self,
        document: DocId,
        ordinal: u32,
        vector: &[f32],
    ) -> StorageBackendResult<()> {
        self.check()?;
        let result = self.push_inner(document, ordinal, vector);
        self.failed = result.is_err();
        result
    }

    fn push_inner(
        &mut self,
        document: DocId,
        ordinal: u32,
        vector: &[f32],
    ) -> StorageBackendResult<()> {
        let key = active_key(document, ordinal);
        if self.after.is_some_and(|previous| key <= previous) {
            return Err(corrupt(
                "canonical vector identities are not strictly ordered",
            ));
        }
        let missing_node = || {
            corrupt(format!(
                "canonical vector {document}:{ordinal} has no live graph node"
            ))
        };
        let Some((expected, node_id)) = self.index.active.next(self.after)? else {
            return Err(missing_node());
        };
        if expected < key {
            return Err(missing_canonical(expected));
        }
        if expected > key {
            return Err(missing_node());
        }
        let node = self.index.raw_vector(*node_id)?;
        if node.values.len() != vector.len()
            || !node
                .values
                .iter()
                .zip(vector)
                .all(|(left, right)| left.to_bits() == right.to_bits())
        {
            return Err(corrupt(format!(
                "canonical vector {document}:{ordinal} differs from its live graph node"
            )));
        }
        self.after = Some(key);
        Ok(())
    }

    pub fn finish(self) -> StorageBackendResult<()> {
        self.check()?;
        if let Some((key, _)) = self.index.active.next(self.after)? {
            return Err(missing_canonical(key));
        }
        Ok(())
    }

    fn check(&self) -> StorageBackendResult<()> {
        self.control.check()?;
        if self.failed {
            return Err(corrupt(
                "canonical validation cannot continue after an error",
            ));
        }
        Ok(())
    }
}

fn missing_canonical(key: u128) -> StorageBackendError {
    corrupt(format!(
        "live graph node {}:{} has no canonical vector",
        key >> 32,
        key as u32
    ))
}

fn corrupt(message: impl std::fmt::Display) -> StorageBackendError {
    StorageBackendError::Other(format!("corrupt HNSW graph: {message}"))
}

#[cfg(test)]
mod tests;
