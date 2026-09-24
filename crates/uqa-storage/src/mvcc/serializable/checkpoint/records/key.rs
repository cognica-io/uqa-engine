//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Fixed-width ordered addresses preserve participant identity and immutable observation contents.

use super::{invalid, VersionResult};

/// An opaque ordered checkpoint address. Providers preserve all 49 bytes and byte ordering; common storage validates the corresponding record and owns its interpretation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SerializableCheckpointKey([u8; 49]);

impl SerializableCheckpointKey {
    pub(super) const HEADER: Self = Self([0; 49]);

    pub fn from_bytes(bytes: &[u8]) -> VersionResult<Self> {
        let key = Self(bytes.try_into().map_err(|_| invalid())?);
        let valid = match key.0[0] {
            0 => key == Self::HEADER,
            1 => key.0[41..].iter().all(|&byte| byte == 0),
            2 => key.0[17..].iter().all(|&byte| byte == 0),
            3 | 4 => true,
            _ => false,
        };
        if !valid {
            return Err(invalid());
        }
        Ok(key)
    }

    pub const fn as_bytes(&self) -> &[u8; 49] {
        &self.0
    }

    pub(super) const fn kind(self) -> u8 {
        self.0[0]
    }

    pub(super) fn transaction(id: u64, fingerprint: [u8; 32]) -> Self {
        let mut key = [0; 49];
        key[0] = 1;
        key[1..9].copy_from_slice(&id.to_be_bytes());
        key[9..41].copy_from_slice(&fingerprint);
        Self(key)
    }

    pub(super) fn edge(reader: u64, writer: u64) -> Self {
        let mut key = [0; 49];
        key[0] = 2;
        key[1..9].copy_from_slice(&reader.to_be_bytes());
        key[9..17].copy_from_slice(&writer.to_be_bytes());
        Self(key)
    }

    pub(super) fn predicate(object: [u8; 16], fingerprint: [u8; 32], writing: bool) -> Self {
        let mut key = [0; 49];
        key[0] = if writing { 4 } else { 3 };
        key[1..17].copy_from_slice(&object);
        key[17..].copy_from_slice(&fingerprint);
        Self(key)
    }
}
