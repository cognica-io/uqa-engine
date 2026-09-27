//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Provider-independent population records and complete publication census. Common MVCC owns atomic reconciliation; providers supply physical layouts.

use super::DiskANNCanonicalCounts;
use crate::diskann_index::{
    canonical::DiskANNQueryRead,
    format::{DiskANNCanonicalOrigin, DiskANNGeneration},
    pages::DiskANNOriginReader,
};
use crate::{mvcc::VersionError, read_control::StorageReadControl, StorageBackendResult};
use uqa_core::DocId;

mod encoding;

/// Exact populations for one canonical view and selected generation. The owner installs this record atomically with all current-origin witnesses and canonical mutations; decoding alone does not establish that association.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiskANNPopulationState {
    generation: DiskANNGeneration,
    dimensions: u32,
    counts: DiskANNCanonicalCounts,
}

/// Explicit coverage evidence for one current document, including empty tensors. A missing witness for an existing origin is corruption, never implicit negative membership.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiskANNPopulationWitness {
    generation: DiskANNGeneration,
    document: DocId,
    origin: DiskANNCanonicalOrigin,
    covered: bool,
}

impl DiskANNPopulationState {
    /// Construct a validated record value. The lifecycle owner still establishes its exact canonical association through census or checked replacement before installation.
    pub fn from_counts(
        generation: DiskANNGeneration,
        dimensions: u32,
        counts: DiskANNCanonicalCounts,
    ) -> StorageBackendResult<Self> {
        if dimensions == 0 {
            return Err(invalid("population dimensions must be nonzero"));
        }
        Ok(Self {
            generation,
            dimensions,
            counts,
        })
    }

    pub fn generation(self) -> DiskANNGeneration {
        self.generation
    }

    pub fn dimensions(self) -> u32 {
        self.dimensions
    }

    pub fn counts(self) -> DiskANNCanonicalCounts {
        self.counts
    }

    /// Enumerate complete current origin metadata against an already verified complete build-origin reader. This is a publication/reconciliation operation, never a planning read. The caller stages every witness and the returned header in one atomic batch and discards that batch on any error.
    pub fn capture(
        current: &dyn DiskANNQueryRead,
        built: &DiskANNOriginReader,
        control: &StorageReadControl,
        visit: &mut dyn FnMut(DiskANNPopulationWitness) -> StorageBackendResult<()>,
    ) -> StorageBackendResult<Self> {
        current.check_control(control)?;
        built.check_control(control)?;
        let input = built.manifest().input();
        if current.dimensions() != input.dimensions {
            return Err(invalid(
                "population source dimensions differ from generation",
            ));
        }
        let mut state = Self {
            generation: input.generation,
            dimensions: input.dimensions,
            counts: DiskANNCanonicalCounts::default(),
        };
        let mut after = None;
        while let Some(document) = current.next_document_after(after, control)? {
            current.check_control(control)?;
            if after.is_some_and(|previous| document <= previous) {
                return Err(invalid("population origin cursor did not advance"));
            }
            let origin = current
                .document_origin(document, control)?
                .ok_or_else(|| invalid("population document has no canonical origin"))?;
            if origin.dimensions() != state.dimensions {
                return Err(invalid("population origin dimensions differ from field"));
            }
            let captured = built.origin(document, control)?;
            let covered = captured.is_some_and(|captured| captured.version() == origin.version());
            if covered && captured != Some(origin) {
                return Err(invalid(
                    "one canonical origin has inconsistent tensor shapes",
                ));
            }
            let vectors = state
                .counts
                .current_vectors()
                .checked_add(origin.count())
                .ok_or_else(|| invalid("population vector count overflow"))?;
            let changed = state
                .counts
                .changed_vectors()
                .checked_add(if covered { 0 } else { origin.count() })
                .ok_or_else(|| invalid("population changed vector count overflow"))?;
            state.counts = DiskANNCanonicalCounts::new(vectors, changed)?;
            current.check_control(control)?;
            visit(DiskANNPopulationWitness {
                generation: state.generation,
                document,
                origin,
                covered,
            })?;
            after = Some(document);
        }
        current.check_control(control)?;
        built.check_control(control)?;
        Ok(state)
    }

    /// Replace an actual current tensor with a globally fresh origin allocated by the mutation owner. The preceding origin and its required witness must come from the same view as this header. A generation change requires reconciliation before applying this operation.
    pub fn replaced(
        self,
        document: DocId,
        previous: Option<(DiskANNCanonicalOrigin, DiskANNPopulationWitness)>,
        replacement: DiskANNCanonicalOrigin,
    ) -> StorageBackendResult<(Self, DiskANNPopulationWitness)> {
        if replacement.dimensions() != self.dimensions {
            return Err(invalid(
                "replacement dimensions differ from population field",
            ));
        }
        let (previous_vectors, previous_changed) = match previous {
            Some((origin, witness)) => {
                witness.validate(self.generation, document, origin)?;
                if origin.dimensions() != self.dimensions
                    || origin.version() == replacement.version()
                {
                    return Err(invalid(
                        "population replacement requires a fresh field origin",
                    ));
                }
                (origin.count(), !witness.covered)
            }
            None => (0, false),
        };
        let counts =
            self.counts
                .replaced(previous_vectors, previous_changed, replacement.count())?;
        Ok((
            Self { counts, ..self },
            DiskANNPopulationWitness {
                generation: self.generation,
                document,
                origin: replacement,
                covered: false,
            },
        ))
    }
}

impl DiskANNPopulationWitness {
    pub fn document(self) -> DocId {
        self.document
    }

    pub fn origin(self) -> DiskANNCanonicalOrigin {
        self.origin
    }

    pub fn covered(self) -> bool {
        self.covered
    }

    fn validate(
        self,
        generation: DiskANNGeneration,
        document: DocId,
        origin: DiskANNCanonicalOrigin,
    ) -> StorageBackendResult<()> {
        if self.generation != generation || self.document != document || self.origin != origin {
            return Err(invalid(
                "population witness does not match current origin and generation",
            ));
        }
        Ok(())
    }
}

fn invalid(message: &'static str) -> crate::StorageBackendError {
    VersionError::InvalidEncoding(message).into_storage_error()
}

#[cfg(test)]
mod tests;
