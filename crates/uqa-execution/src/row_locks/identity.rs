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
    DocumentIdentityReservations(Arc<str>),
    BackendWriter,
    KeyReservation([u8; 32]),
    ScoringParameters(Arc<str>),
    SharedObject { class_id: u32, oid: u32 },
    SharedObjectName { class_id: u32, name: Arc<str> },
    SharedObjectTuple { class_id: u32, oid: u32 },
}

impl LockRelationIdentity {
    pub(super) fn stable_bytes(&self) -> Vec<u8> {
        match self {
            Self::Table(name) => name.as_bytes().to_vec(),
            Self::DocumentIdentityReservations(name) => {
                let mut bytes = Vec::with_capacity(1 + "document-identities".len() + name.len());
                bytes.extend_from_slice(b"\xffdocument-identities");
                bytes.extend_from_slice(name.as_bytes());
                bytes
            }
            Self::BackendWriter => b"\xffbackend-writer".to_vec(),
            Self::ScoringParameters(name) => {
                let mut bytes = b"\xffscoring-parameters".to_vec();
                bytes.extend_from_slice(name.as_bytes());
                bytes
            }
            Self::KeyReservation(digest) => {
                let mut bytes = Vec::with_capacity(1 + "key-reservation".len() + digest.len());
                bytes.extend_from_slice(b"\xffkey-reservation");
                bytes.extend_from_slice(digest);
                bytes
            }
            Self::SharedObject { class_id, oid } => {
                let mut bytes = b"\xffshared-object".to_vec();
                bytes.extend_from_slice(&class_id.to_be_bytes());
                bytes.extend_from_slice(&oid.to_be_bytes());
                bytes
            }
            Self::SharedObjectName { class_id, name } => {
                let mut bytes = b"\xffshared-object-name".to_vec();
                bytes.extend_from_slice(&class_id.to_be_bytes());
                bytes.extend_from_slice(name.as_bytes());
                bytes
            }
            Self::SharedObjectTuple { class_id, oid } => {
                let mut bytes = b"\xffshared-object-tuple".to_vec();
                bytes.extend_from_slice(&class_id.to_be_bytes());
                bytes.extend_from_slice(&oid.to_be_bytes());
                bytes
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RowLockKey {
    pub table: u64,
    pub doc_id: DocId,
}
