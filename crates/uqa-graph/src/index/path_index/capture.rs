//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capture materialized reachability without evaluating paths against another graph generation.

use super::{PathIndex, PathStorage};
use uqa_core::memory::{Budgeted, ProductionControl, ProductionVec};
use uqa_storage::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};

#[derive(Debug, PartialEq, Eq)]
pub struct CapturedPath {
    pub sequence: Vec<String>,
    pub pairs: Vec<(u64, u64)>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct CapturedPathIndex {
    pub paths: Vec<CapturedPath>,
}

impl PathIndex {
    /// Detach exact ordered label sequences and evaluated pairs under the capture allowance. Persistent indexes and query views belong to their own storage snapshot.
    pub fn capture_memory_with_control(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Budgeted<CapturedPathIndex>> {
        control.check()?;
        let PathStorage::Memory(paths) = &self.storage else {
            return Err(StorageBackendError::Other(
                "memory path-index capture requires materialized memory storage".into(),
            ));
        };
        let production = ProductionControl::new(
            control.memory(),
            control.cancellation(),
            control.cancellation(),
        );
        let mut output = ProductionVec::new(production);
        output.reserve(paths.image.paths.len())?;
        for CapturedPath { sequence, pairs } in &paths.image.paths {
            let mut labels = ProductionVec::new(production);
            labels.reserve(sequence.len())?;
            for label in sequence {
                labels.push_produced(production.copy_text(label)?)?;
            }
            let mut copied = ProductionVec::new(production);
            copied.reserve(pairs.len())?;
            for pair in pairs {
                copied.push_copy(*pair)?;
            }
            let (sequence, labels_memory) = labels.finish()?.into_parts();
            let (pairs, pairs_memory) = copied.finish()?.into_parts();
            output.push_produced(production.finish(
                CapturedPath { sequence, pairs },
                production.combine(labels_memory, pairs_memory),
            )?)?;
        }
        let (paths, memory) = output.finish()?.into_parts();
        Ok(production
            .finish(CapturedPathIndex { paths }, memory)?
            .into_budgeted()
            .unwrap_or_else(|_| unreachable!("path capture owns an allowance")))
    }
}

#[cfg(test)]
mod tests;
