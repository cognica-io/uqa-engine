//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Private canonical build capture. A coverage fingerprint does not verify the caller's selected snapshot or authorize generation publication.

use std::path::Path;

use uqa_core::{memory::BudgetedVec, DocId};

use super::format::{
    DiskANNBuildCoverage, DiskANNCoverageBuilder, DiskANNGeneration, DiskANNVectorVersion,
};
use super::{ExactVectorReason, NavigationInput, PQCodebook, PQTrainer, PQTrainingOptions};
use crate::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};

mod merge;
mod partitions;
mod records;
mod runs;
mod temporary;
#[cfg(test)]
mod tests;

pub use merge::{DiskANNMergeOptions, DiskANNMergeSummary, DiskANNMergedGraph};
pub use partitions::{DiskANNPartitionOptions, DiskANNPartitionRuns, DiskANNPartitionSummary};
pub use temporary::{DiskANNTemporaryBudget, DiskANNTemporaryError};

use records::Records;

pub type DiskANNBuildVisitor<'a> =
    dyn FnMut(DocId, u32, DiskANNVectorVersion, &[f32]) -> StorageBackendResult<()> + 'a;

/// One captured canonical vector. Its original bits and version are independent of navigation, payloads and scores; its raw buffer retains the build allowance.
#[derive(Debug)]
pub struct DiskANNBuildVector {
    doc: DocId,
    ordinal: u32,
    version: DiskANNVectorVersion,
    raw: BudgetedVec<f32>,
    exact: Option<ExactVectorReason>,
}

impl DiskANNBuildVector {
    pub fn doc_id(&self) -> DocId {
        self.doc
    }
    pub fn ordinal(&self) -> u32 {
        self.ordinal
    }
    pub fn version(&self) -> DiskANNVectorVersion {
        self.version
    }
    pub fn raw(&self) -> &[f32] {
        &self.raw
    }
    pub fn exact_reason(&self) -> Option<ExactVectorReason> {
        self.exact
    }
}

/// Repeatable encrypted input, with fixed-width navigation and numeric-side records. Dense navigation IDs follow canonical logical order without a resident global directory.
pub struct DiskANNBuildInput {
    dimensions: u32,
    navigation: Records,
    side: Records,
    coverage: DiskANNBuildCoverage,
    control: StorageReadControl,
    temporary: DiskANNTemporaryBudget,
}

impl DiskANNBuildInput {
    /// Enumerate every selected canonical ordinal once in document order. Any source or consumer failure discards the entire capture, including failures the source suppresses; temporary data is never a staged or published generation.
    pub fn capture(
        generation: DiskANNGeneration,
        dimensions: u32,
        directory: &Path,
        temporary: &DiskANNTemporaryBudget,
        control: &StorageReadControl,
        read: impl FnOnce(&mut DiskANNBuildVisitor<'_>) -> StorageBackendResult<()>,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        let mut coverage = DiskANNCoverageBuilder::new(generation, dimensions)?;
        let mut navigation = Records::new(dimensions, false, directory, temporary, control)?;
        let mut side = Records::new(dimensions, true, directory, temporary, control)?;
        let mut first_error = None;
        let result = read(&mut |doc, ordinal, version, raw| {
            if first_error.is_none() {
                let result = (|| {
                    coverage.push(doc, ordinal, version, raw, control)?;
                    let (norm, _) = super::metric::norms(dimensions, raw, control)?;
                    let destination = if super::metric::exact_reason(norm).is_some() {
                        &mut side
                    } else {
                        &mut navigation
                    };
                    destination.push(doc, ordinal, version, raw, control)
                })();
                if let Err(error) = result {
                    first_error = Some(error);
                }
            }
            if first_error.is_some() {
                Err(invalid("canonical input consumer rejected a vector"))
            } else {
                Ok(())
            }
        });
        if let Some(error) = first_error {
            return Err(error);
        }
        result?;
        control.check()?;
        Ok(Self {
            dimensions,
            navigation,
            side,
            coverage: coverage.finish(),
            control: control.clone(),
            temporary: temporary.clone(),
        })
    }

    pub fn dimensions(&self) -> u32 {
        self.dimensions
    }
    pub fn node_count(&self) -> u64 {
        self.navigation.len()
    }
    pub fn side_count(&self) -> u64 {
        self.side.len()
    }
    pub fn coverage(&self) -> DiskANNBuildCoverage {
        self.coverage
    }

    pub fn read_node(&self, node: u64) -> StorageBackendResult<DiskANNBuildVector> {
        self.navigation.read(node, &self.control)
    }

    pub fn read_side(&self, index: u64) -> StorageBackendResult<DiskANNBuildVector> {
        self.side.read(index, &self.control)
    }

    /// Train the existing bounded reservoir by replaying captured navigation values. Empty and all-side inputs have no codebook; invalid settings still fail validation.
    pub fn train(
        &self,
        pq_bytes: usize,
        options: PQTrainingOptions,
    ) -> StorageBackendResult<Option<PQCodebook>> {
        let mut trainer = PQTrainer::new(self.dimensions, pq_bytes, options, &self.control)?;
        self.navigation.visit(&self.control, &mut |record| {
            let NavigationInput::Navigable(vector) =
                NavigationInput::from_raw(self.dimensions, record.raw(), &self.control)?
            else {
                return Err(invalid("navigation record changed classification"));
            };
            trainer.observe(&vector)
        })?;
        if self.node_count() == 0 {
            return Ok(None);
        }
        trainer.finish().map(Some)
    }
}

fn invalid(message: &str) -> StorageBackendError {
    StorageBackendError::Other(format!("invalid DiskANN build input: {message}"))
}
