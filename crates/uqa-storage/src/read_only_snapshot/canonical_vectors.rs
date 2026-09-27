//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable vector facets share captured data, allocation leases and one snapshot identity.

use super::ReadOnlySnapshot;
use crate::{read_control::StorageReadControl, vector_index::VectorRead, StorageBackendResult};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use uqa_core::{
    memory::{Budgeted, BudgetedVec},
    DocId,
};

struct CanonicalVectors<T: ?Sized> {
    owner: ReadOnlySnapshot<T>,
    fingerprint: [u8; 32],
}

impl<T: VectorRead + 'static> ReadOnlySnapshot<T> {
    /// Expose canonical values from an already captured immutable owner without copying coordinates or its directory. Cache owners may remain unbound; each reader later binds its own cancellation. A supplied control admits the facet header, while None preserves the existing uncontrolled snapshot API. The unique view identity is conservative across independently captured equal corpora and stable across shared/nested readers.
    pub fn with_canonical_vectors(
        mut self,
        control: Option<&StorageReadControl>,
    ) -> StorageBackendResult<Self> {
        if let Some(control) = control {
            control.check()?;
        }
        if self.3.is_some() {
            return Ok(self);
        }
        let identity =
            crate::catalog::new_nonzero_catalog_identity("vector snapshot", "canonical values")?;
        let mut digest = Sha256::new();
        digest.update(b"uqa-immutable-vector-view-v1\0");
        digest.update(identity);
        // This clone has no canonical facet yet: the ownership graph remains acyclic.
        let source = CanonicalVectors {
            owner: self.clone(),
            fingerprint: digest.finalize().into(),
        };
        self.3 = Some(match control {
            Some(control) => {
                Budgeted::new(source, control.memory().empty_reservation()).into_shared()?
            }
            None => Arc::new(source),
        });
        Ok(self)
    }
}

impl<T: VectorRead + ?Sized> VectorRead for CanonicalVectors<T> {
    fn check_control(&self, control: &StorageReadControl) -> StorageBackendResult<()> {
        self.owner.check_control(control)
    }
    fn dimensions(&self) -> u32 {
        self.owner.dimensions()
    }
    fn corpus_fingerprint(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<[u8; 32]>> {
        self.check_control(control)?;
        Ok(Some(self.fingerprint))
    }
    fn next_document_after(
        &self,
        after: Option<DocId>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DocId>> {
        self.owner.next_document_after(after, control)
    }
    fn document_vector_count(
        &self,
        document: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<u64> {
        self.owner.document_vector_count(document, control)
    }
    fn read_vector(
        &self,
        document: DocId,
        ordinal: u32,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<BudgetedVec<f32>>> {
        self.owner.read_vector(document, ordinal, control)
    }
}
