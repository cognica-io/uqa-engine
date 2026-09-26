//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Build membership comes from the retained canonical source, not its fingerprint or writer allocation order.

use std::path::Path;

use super::{invalid, DiskANNBuildInput, DiskANNTemporaryBudget};
use crate::diskann_index::{
    format::{DiskANNBuildCoverage, DiskANNChangeIdentity, DiskANNGeneration, DiskANNManifest},
    DiskANNCanonicalRead,
};
use crate::{read_control::StorageReadControl, StorageBackendResult};

#[cfg(test)]
mod tests;

/// Encrypted build input and the exact committed/private source used to produce it. Moving the source keeps its original leases and controls alive through construction.
pub struct DiskANNBuildCapture<S> {
    input: DiskANNBuildInput,
    source: S,
}

impl<S: DiskANNCanonicalRead> DiskANNBuildCapture<S> {
    pub fn capture(
        generation: DiskANNGeneration,
        source: S,
        directory: &Path,
        temporary: &DiskANNTemporaryBudget,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        source.check_control(control)?;
        let input =
            DiskANNBuildInput::capture_source(generation, &source, directory, temporary, control)?;
        source.check_control(control)?;
        Ok(Self { input, source })
    }

    pub fn input(&self) -> &DiskANNBuildInput {
        &self.input
    }

    /// Bind the completed manifest to this capture and release temporary input. Physical sealing, provider/index binding and atomic publication still belong to their storage owners.
    pub fn finish(
        self,
        manifest: &DiskANNManifest,
        control: &StorageReadControl,
    ) -> StorageBackendResult<DiskANNCanonicalCoverage<S>> {
        self.input.control.check()?;
        self.source.check_control(control)?;
        let actual = manifest.input();
        if actual.coverage != self.input.coverage()
            || actual.nodes != self.input.node_count()
            || actual.side_vectors != self.input.side_count()
        {
            return Err(invalid(
                "manifest does not match the retained build capture",
            ));
        }
        control.check()?;
        Ok(DiskANNCanonicalCoverage {
            source: self.source,
            fingerprint: self.input.coverage(),
            control: self.input.control.clone(),
        })
    }
}

/// Exact origin membership on one completed build's retained source, including empty tensors. This process-owned evidence cannot be reconstructed from a manifest hash after losing its source.
pub struct DiskANNCanonicalCoverage<S> {
    source: S,
    fingerprint: DiskANNBuildCoverage,
    control: StorageReadControl,
}

impl<S: DiskANNCanonicalRead> DiskANNCanonicalCoverage<S> {
    pub fn fingerprint(&self) -> DiskANNBuildCoverage {
        self.fingerprint
    }

    /// Preserve the concrete provider source for its owner to validate table/field identity before consuming coverage in a publication or retirement operation.
    pub fn source(&self) -> &S {
        &self.source
    }

    /// A change is covered exactly when this source selects its complete canonical tensor with the same original writer and mutation revision. Neither absence nor an earlier writer allocation implies coverage.
    pub fn contains(
        &self,
        change: DiskANNChangeIdentity,
        control: &StorageReadControl,
    ) -> StorageBackendResult<bool> {
        self.control.check()?;
        self.source.check_control(control)?;
        let actual = self.source.origin(change.document(), control)?;
        self.control.check()?;
        self.source.check_control(control)?;
        Ok(actual == Some(change.version()))
    }
}
