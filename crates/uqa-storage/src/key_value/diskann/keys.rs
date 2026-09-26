//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::diskann_index::{format::DiskANNGeneration, pages::DiskANNRecordKey};
use crate::StorageBackendResult;

use super::invalid;

pub(super) const ROOT: &[u8] = b"\0uqa-diskann-v1\0";
pub(super) const PREFIX_BYTES: usize = ROOT.len() + 1 + 40;
pub(super) const KEY_BYTES: usize = PREFIX_BYTES + 9;

pub(super) fn database_key() -> [u8; ROOT.len() + 1] {
    let mut key = [0; ROOT.len() + 1];
    key[..ROOT.len()].copy_from_slice(ROOT);
    key
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    State,
    Record(DiskANNRecordKey),
    Graph(u64),
}

#[derive(Clone, Copy)]
pub(super) struct Key {
    bytes: [u8; KEY_BYTES],
    len: usize,
}

impl AsRef<[u8]> for Key {
    fn as_ref(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

#[derive(Clone, Copy)]
pub(super) struct Keys {
    prefix: [u8; PREFIX_BYTES],
}

impl Keys {
    pub(super) fn new(generation: DiskANNGeneration) -> Self {
        let mut prefix = [0; PREFIX_BYTES];
        prefix[..ROOT.len()].copy_from_slice(ROOT);
        prefix[ROOT.len()] = 1;
        let start = ROOT.len() + 1;
        prefix[start..start + 16].copy_from_slice(&generation.database());
        for (index, value) in [
            generation.table(),
            generation.index(),
            generation.generation(),
        ]
        .into_iter()
        .enumerate()
        {
            let offset = start + 16 + 8 * index;
            prefix[offset..offset + 8].copy_from_slice(&value.to_be_bytes());
        }
        Self { prefix }
    }

    pub(super) fn prefix(&self) -> &[u8] {
        &self.prefix
    }

    pub(super) fn allocation_namespace(&self) -> &[u8] {
        &self.prefix[..PREFIX_BYTES - 8]
    }

    pub(super) fn key(self, kind: Kind) -> Key {
        let (tag, suffix) = match kind {
            Kind::State => (0, None),
            Kind::Record(DiskANNRecordKey::Manifest) => (1, None),
            Kind::Record(DiskANNRecordKey::Codebook) => (2, None),
            Kind::Record(DiskANNRecordKey::Codes(first)) => (3, Some(first)),
            Kind::Record(DiskANNRecordKey::Side(first)) => (4, Some(first)),
            Kind::Graph(page) => (5, Some(page)),
        };
        let mut bytes = [0; KEY_BYTES];
        bytes[..PREFIX_BYTES].copy_from_slice(&self.prefix);
        bytes[PREFIX_BYTES] = tag;
        if let Some(value) = suffix {
            bytes[PREFIX_BYTES + 1..].copy_from_slice(&value.to_be_bytes());
        }
        Key {
            bytes,
            len: PREFIX_BYTES + 1 + usize::from(suffix.is_some()) * 8,
        }
    }

    pub(super) fn decode(self, bytes: &[u8]) -> StorageBackendResult<Kind> {
        if !bytes.starts_with(&self.prefix) {
            return Err(invalid("key belongs to another generation"));
        }
        let tail = &bytes[PREFIX_BYTES..];
        match tail {
            [0] => Ok(Kind::State),
            [1] => Ok(Kind::Record(DiskANNRecordKey::Manifest)),
            [2] => Ok(Kind::Record(DiskANNRecordKey::Codebook)),
            [tag @ 3..=5, suffix @ ..] if suffix.len() == 8 => {
                let position = u64::from_be_bytes(suffix.try_into().expect("eight-byte position"));
                Ok(match tag {
                    3 => Kind::Record(DiskANNRecordKey::Codes(position)),
                    4 => Kind::Record(DiskANNRecordKey::Side(position)),
                    _ => Kind::Graph(position),
                })
            }
            _ => Err(invalid("unknown record kind or key length")),
        }
    }
}
