//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Materialized owners lend ordered raw tensors without copying their corpus at capture.

use crate::{read_control::StorageReadControl, StorageBackendResult};
use uqa_core::memory::BudgetedVec;

pub(crate) fn ordinal_count(
    ordinals: impl Iterator<Item = u32>,
    control: &StorageReadControl,
) -> StorageBackendResult<u64> {
    control.check()?;
    let mut count = 0;
    for ordinal in ordinals {
        control.check()?;
        if u64::from(ordinal) != count {
            return Err(crate::mvcc::VersionError::InvalidEncoding(
                "noncontiguous canonical vector ordinals",
            )
            .into_storage_error());
        }
        count += 1;
    }
    Ok(count)
}

pub(crate) fn copy_vector(
    source: &[f32],
    control: &StorageReadControl,
) -> StorageBackendResult<BudgetedVec<f32>> {
    control.check()?;
    let mut copied = BudgetedVec::new(control.memory());
    copied.reserve(source.len())?;
    for chunk in source.chunks(1024) {
        control.check()?;
        copied.extend_from_slice(chunk)?;
    }
    control.check()?;
    Ok(copied)
}
