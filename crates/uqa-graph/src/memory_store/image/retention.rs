//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered image keys cannot repeat or move backwards.

use super::{invalid, StorageBackendResult};

pub(super) fn ordered<K: Ord>(previous: Option<&K>, key: &K) -> StorageBackendResult<()> {
    if previous.is_some_and(|previous| previous >= key) {
        Err(invalid())
    } else {
        Ok(())
    }
}

pub(super) async fn ordered_text(
    previous: Option<&String>,
    key: &str,
    input: &mut uqa_core::image_io::AsyncImageInput<'_>,
) -> StorageBackendResult<()> {
    let Some(previous) = previous else {
        return Ok(());
    };
    for (left, right) in previous
        .as_bytes()
        .chunks(4096)
        .zip(key.as_bytes().chunks(4096))
    {
        input.checkpoint().await?;
        match left.cmp(right) {
            std::cmp::Ordering::Less => return Ok(()),
            std::cmp::Ordering::Greater => return Err(invalid()),
            std::cmp::Ordering::Equal => (),
        }
    }
    if previous.len() < key.len() {
        Ok(())
    } else {
        Err(invalid())
    }
}
