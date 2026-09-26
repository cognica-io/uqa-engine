//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Field transitions fence vector writers while ordinary document writers remain mergeable.

use super::{KeyValueBatch, KeyValueVectorIndex, StorageBackendResult};
use uqa_core::memory::{BudgetedVec, MemoryError};

pub(crate) const ROOT: &[u8] = b"\0uqa-vector-field-guards-v1\0";

pub struct KeyValueVectorFieldGuards;

impl crate::mvcc::VectorFieldGuardLayout for KeyValueVectorFieldGuards {
    fn prefix(
        &self,
        control: &crate::read_control::StorageReadControl,
    ) -> crate::mvcc::VersionResult<BudgetedVec<u8>> {
        let mut prefix = BudgetedVec::new(control.memory());
        prefix.extend_from_slice(ROOT)?;
        Ok(prefix)
    }

    fn reference(
        &self,
        key: &[u8],
        control: &crate::read_control::StorageReadControl,
    ) -> crate::mvcc::VersionResult<Option<crate::mvcc::VectorFieldGuard>> {
        use crate::key_value::codec::read_segment;
        use crate::mvcc::{VectorFieldGuard, VersionError};

        control.check()?;
        let suffix = key
            .strip_prefix(ROOT)
            .ok_or(VersionError::InvalidEncoding("foreign vector field guard"))?;
        let (&kind, vectors) = suffix.split_first().ok_or(VersionError::InvalidEncoding(
            "truncated vector field guard",
        ))?;
        if kind > 1 || vectors.first() != Some(&crate::key_value::TAG_VECTOR) {
            return Err(VersionError::InvalidEncoding("invalid vector field guard"));
        }
        let mut offset = 1;
        for _ in 0..2 {
            std::str::from_utf8(read_segment(vectors, &mut offset)?).map_err(|_| {
                VersionError::InvalidEncoding("vector field guard name is not UTF-8")
            })?;
        }
        if offset != vectors.len() {
            return Err(VersionError::InvalidEncoding(
                "vector field guard has trailing bytes",
            ));
        }
        if kind == 0 {
            return Ok(None);
        }
        let copy = |bytes: &[u8]| -> crate::mvcc::VersionResult<BudgetedVec<u8>> {
            let mut output = BudgetedVec::new(control.memory());
            output.extend_from_slice(bytes)?;
            Ok(output)
        };
        let mut lifetime = copy(key)?;
        lifetime[ROOT.len()] = 0;
        Ok(Some(VectorFieldGuard {
            lifetime,
            references: copy(key)?,
            reference_value: copy(&[1])?,
            vectors: copy(vectors)?,
        }))
    }
}

impl KeyValueVectorIndex {
    pub(super) fn coordinate_field(
        &self,
        batch: &mut dyn KeyValueBatch,
        structural: bool,
    ) -> StorageBackendResult<()> {
        if !self.store.transaction_model().is_versioned() {
            return Ok(());
        }
        let control = self.store.retention_control().ok_or_else(|| {
            super::other_error("versioned vector owner has no retention allowance")
        })?;
        control.check()?;
        let bytes = self
            .table
            .len()
            .checked_add(self.field.len())
            .and_then(|size| size.checked_add(32))
            .and_then(|size| size.checked_mul(2))
            .ok_or(MemoryError::SizeOverflow)?;
        let _workspace = control.memory().reserve(bytes)?;
        let prefix = super::vector_field_prefix(&self.table, &self.field)?;
        let mut lifetime = BudgetedVec::new(control.memory());
        lifetime.extend_from_slice(ROOT)?;
        lifetime.push(0)?;
        lifetime.extend_from_slice(&prefix)?;
        let mut references = BudgetedVec::new(control.memory());
        references.extend_from_slice(ROOT)?;
        references.push(1)?;
        references.extend_from_slice(&prefix)?;
        if structural {
            batch.fence_record(&lifetime)?;
            batch.fence_record(&references)
        } else {
            batch.require_unchanged(&lifetime)?;
            batch.touch_marker(&references, &[1])
        }
    }
}
