//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog name comparisons retain progress through long common prefixes.

use crate::StorageBackendResult;
use std::cmp::Ordering;
use uqa_core::image_io::AsyncImageInput;

/// Compare catalog identities through an enclosing input continuation, including long common prefixes.
pub async fn compare_names(
    left: &str,
    right: &str,
    progress: &mut AsyncImageInput<'_>,
) -> StorageBackendResult<Ordering> {
    compare_with(left, right, progress).await
}

pub(super) async fn compare_with(
    left: &str,
    right: &str,
    progress: &mut impl crate::image_io::work::Work,
) -> StorageBackendResult<Ordering> {
    for (left, right) in left
        .as_bytes()
        .chunks(4096)
        .zip(right.as_bytes().chunks(4096))
    {
        progress.checkpoint().await?;
        let order = left.cmp(right);
        if order != Ordering::Equal {
            return Ok(order);
        }
    }
    Ok(left.len().cmp(&right.len()))
}
