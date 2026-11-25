//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Explicit, non-serializable key propagation between persistent storage owners.

use std::sync::Arc;
use std::{fmt, path::Path};
use zeroize::Zeroizing;

/// Encryption credential for database-owned auxiliary storage. Drivers validate
/// the credential when opening storage; cloning this handle shares its bytes.
/// Debug output never includes the credential.
#[derive(Clone)]
pub struct StorageEncryptionKey {
    secret: Arc<Zeroizing<String>>,
    files: Option<Arc<dyn AuxiliaryFileKeys>>,
}

/// Database-owned files that can have independent encryption keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuxiliaryFile {
    Notifications,
    RelationIdentities,
    Serializable,
}

impl AuxiliaryFile {
    /// The adjacent file selected by this database's durable coordination protocol.
    pub fn path(self, database: &Path) -> std::path::PathBuf {
        let mut path = database.as_os_str().to_owned();
        path.push(match self {
            Self::Notifications => ".uqa-notification-state",
            Self::RelationIdentities => ".uqa-relation-identities",
            Self::Serializable => ".uqa-serializable",
        });
        path.into()
    }
}

/// Select the credential for one database-owned file, including existing file formats.
pub trait AuxiliaryFileKeys: Send + Sync {
    fn file_key(
        &self,
        file: AuxiliaryFile,
        path: &Path,
    ) -> crate::StorageBackendResult<StorageEncryptionKey>;
}

impl StorageEncryptionKey {
    #[must_use]
    pub fn new(key: &str) -> Self {
        Self {
            secret: Arc::new(Zeroizing::new(key.to_owned())),
            files: None,
        }
    }

    /// Attach a provider's per-file key selection to the original credential.
    #[must_use]
    pub fn with_file_keys(&self, files: Arc<dyn AuxiliaryFileKeys>) -> Self {
        Self {
            secret: self.secret.clone(),
            files: Some(files),
        }
    }

    /// Providers with one shared credential use it for each file.
    pub fn for_file(&self, file: AuxiliaryFile, path: &Path) -> crate::StorageBackendResult<Self> {
        self.files
            .as_ref()
            .map_or_else(|| Ok(self.clone()), |source| source.file_key(file, path))
    }

    /// Expose the credential only to configure a storage driver's encryption.
    #[must_use]
    pub fn expose_secret(&self) -> &str {
        self.secret.as_str()
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
        assert!(Arc::ptr_eq(&key.secret, &cloned.secret));
        assert_eq!(cloned.expose_secret(), "storage-key-regression-marker");
        assert_eq!(format!("{cloned:?}"), "StorageEncryptionKey([REDACTED])");
    }
}
