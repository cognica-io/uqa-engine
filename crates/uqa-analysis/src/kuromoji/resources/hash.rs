//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Artifact and exact rule-source hashes.

use sha2::{Digest, Sha256};

crate::morphology::identity::dictionary_digest!(
    ResourceHash,
    "resource hash",
    crate::kuromoji::DictionaryError
);

impl ResourceHash {
    pub(crate) const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub(crate) const fn into_bytes(self) -> [u8; 32] {
        self.0
    }

    pub fn of(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }
}
