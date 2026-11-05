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
