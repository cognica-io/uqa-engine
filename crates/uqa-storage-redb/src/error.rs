//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_storage::StorageBackendError;

pub(crate) fn redb_error(
    source: impl std::error::Error + Send + Sync + 'static,
) -> StorageBackendError {
    StorageBackendError::backend("redb", source)
}
