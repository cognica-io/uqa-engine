//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Explicit, non-serializable key propagation between persistent storage owners.

use std::fmt;
use std::sync::Arc;

/// Encryption credential for database-owned auxiliary storage. Drivers validate
/// the credential when opening storage; cloning this handle shares its bytes.
/// Debug output never includes the credential.
#[derive(Clone)]
pub struct StorageEncryptionKey(Arc<str>);

impl StorageEncryptionKey {
    #[must_use]
    pub fn new(key: &str) -> Self {
        Self(Arc::from(key))
    }

    /// Expose the credential only to configure a storage driver's encryption.
    #[must_use]
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for StorageEncryptionKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("StorageEncryptionKey([REDACTED])")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloned_key_shares_bytes_without_disclosing_them_in_debug() {
        let key = StorageEncryptionKey::new("storage-key-regression-marker");
        let cloned = key.clone();
        assert!(Arc::ptr_eq(&key.0, &cloned.0));
        assert_eq!(cloned.expose_secret(), "storage-key-regression-marker");
        assert_eq!(format!("{cloned:?}"), "StorageEncryptionKey([REDACTED])");
    }
}
