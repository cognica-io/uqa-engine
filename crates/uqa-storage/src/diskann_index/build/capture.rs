//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Build membership comes from the retained canonical source, not its fingerprint or writer allocation order.

use std::path::Path;

use super::{
    invalid, DiskANNBuildInput, DiskANNBuildSink, DiskANNGenerationOptions, DiskANNMergedGraph,
    DiskANNTemporaryBudget,
};
use crate::diskann_index::{
    format::{
        DiskANNBuildCoverage, DiskANNChangeIdentity, DiskANNGeneration, DiskANNManifest,
        DiskANNOriginSummary,
    },
    DiskANNCanonicalRead,
};
use crate::{read_control::StorageReadControl, StorageBackendResult};

mod origins;
#[cfg(test)]
mod tests;

/// Encrypted build input and the exact committed/private source used to produce it. Moving the source keeps its original leases and controls alive through construction.
pub struct DiskANNBuildCapture<S> {
    input: DiskANNBuildInput,
    source: S,
    origins: origins::Origins,
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
        let mut origins = origins::Origins::new(directory, temporary, control)?;
        let input = DiskANNBuildInput::capture(
            generation,
            source.dimensions(),
            directory,
            temporary,
            control,
            |visit| origins.capture(&source, control, visit),
        )?;
        source.check_control(control)?;
        Ok(Self {
            input,
            source,
            origins,
        })
    }

    pub fn input(&self) -> &DiskANNBuildInput {
        &self.input
    }

    /// Write graph/side artifacts and every captured document origin to the same unpublished generation. Sealing and publication remain explicit owner operations.
    pub fn write_generation(
        &self,
        graph: &DiskANNMergedGraph,
        options: DiskANNGenerationOptions,
        sink: &mut dyn DiskANNBuildSink,
    ) -> StorageBackendResult<DiskANNManifest> {
        let control = &self.input.control;
        self.source.check_control(control)?;
        control.check_value_size(DiskANNManifest::MAX_ENCODED_BYTES, options.max_record_bytes)?;
        self.origins.check_record_limit(
            self.input.coverage().generation(),
            self.input.dimensions(),
            options.max_record_bytes,
            control,
        )?;
        let manifest = self.input.write_generation(graph, options, sink)?;
        self.origins.write(
            self.input.coverage().generation(),
            self.input.dimensions(),
            sink,
            options.max_record_bytes,
            control,
        )?;
        self.source.check_control(control)?;
        manifest.with_origins(self.origins.summary())
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
            || manifest
                .origins()
                .is_some_and(|origins| origins != self.origins.summary())
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
            origins: self.origins.summary(),
            manifest: *manifest,
        })
    }
}

/// Exact origin membership on one completed build's retained source, including empty tensors. This process-owned evidence cannot be reconstructed from a manifest hash after losing its source.
pub struct DiskANNCanonicalCoverage<S> {
    source: S,
    fingerprint: DiskANNBuildCoverage,
    control: StorageReadControl,
    origins: DiskANNOriginSummary,
    manifest: DiskANNManifest,
}

impl<S: DiskANNCanonicalRead> DiskANNCanonicalCoverage<S> {
    /// Preserve the original capture's controls independently of later operation allowances.
    pub fn check_control(&self, control: &StorageReadControl) -> StorageBackendResult<()> {
        self.control.check()?;
        self.source.check_control(control)
    }

    /// The exact manifest accepted by this capture, including separate graph/side counts and artifact identities. Publication must compare it with the actual sealed record.
    pub fn manifest(&self) -> &DiskANNManifest {
        &self.manifest
    }

    pub fn fingerprint(&self) -> DiskANNBuildCoverage {
        self.fingerprint
    }

    /// Expected complete origin artifact from the original capture, including empty tensors. Publication must match it to the actually sealed manifest while this source is retained.
    pub fn origins(&self) -> DiskANNOriginSummary {
        self.origins
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
