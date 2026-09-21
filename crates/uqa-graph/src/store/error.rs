//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain provider diagnostics through cloneable graph errors.

use std::{error::Error, fmt, sync::Arc};
use uqa_storage::StorageBackendError;

/// A retained storage cause. Clones share the original error; equality compares that retained identity, not its display text.
#[derive(Debug, Clone)]
pub struct GraphStorageError(Arc<StorageBackendError>);

impl From<StorageBackendError> for GraphStorageError {
    fn from(error: StorageBackendError) -> Self {
        Self(Arc::new(error))
    }
}

impl PartialEq for GraphStorageError {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for GraphStorageError {}

impl fmt::Display for GraphStorageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self.0.as_ref(), formatter)
    }
}

impl Error for GraphStorageError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.0.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{cypher::CypherError, GraphStoreError};

    #[test]
    fn cloned_graph_errors_retain_the_original_typed_provider_cause() {
        let error = GraphStoreError::from(StorageBackendError::backend(
            "fixture",
            uqa_core::memory::MemoryError::Limit {
                required: 4096,
                limit: 32,
            },
        ));
        let retained = error.clone();
        assert_eq!(retained, error);
        let original = error.source().unwrap().source().unwrap();
        let cypher = CypherError::from(retained);
        let stored = cypher.source().unwrap().source().unwrap();
        assert!(std::ptr::eq(original, stored));
        drop(error);
        let provider = stored.downcast_ref::<StorageBackendError>().unwrap();
        let memory = provider
            .source()
            .unwrap()
            .downcast_ref::<uqa_core::memory::MemoryError>()
            .unwrap();
        assert!(matches!(
            memory,
            uqa_core::memory::MemoryError::Limit {
                required: 4096,
                limit: 32
            }
        ));
    }
}
