//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stable in-process and cross-process lock identities.

use super::{Arc, DocId};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum ManagerIdentity {
    Durable(uqa_storage::PersistentStorageIdentity),
    Provider(usize),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum LockRelationIdentity {
    Table(Arc<str>),
    BackendWriter,
    KeyReservation([u8; 32]),
}

impl LockRelationIdentity {
    pub(super) fn stable_bytes(&self) -> Vec<u8> {
        match self {
            Self::Table(name) => name.as_bytes().to_vec(),
            Self::BackendWriter => b"\xffbackend-writer".to_vec(),
            Self::KeyReservation(digest) => {
                let mut bytes = Vec::with_capacity(1 + "key-reservation".len() + digest.len());
                bytes.extend_from_slice(b"\xffkey-reservation");
                bytes.extend_from_slice(digest);
                bytes
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct RowLockKey {
    pub table: u64,
    pub doc_id: DocId,
}
