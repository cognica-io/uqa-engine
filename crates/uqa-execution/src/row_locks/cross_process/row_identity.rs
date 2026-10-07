//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact claim identities. Hashes choose probe positions, never equality.

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(in crate::row_locks) enum RowIdentity {
    Relation { generation: u64, doc_id: u64 },
    Key { digest: [u8; 32], doc_id: u64 },
}

impl RowIdentity {
    pub(in crate::row_locks) fn encode(self) -> [u8; 48] {
        let mut bytes = [0; 48];
        match self {
            Self::Relation { generation, doc_id } => {
                bytes[0] = 1;
                bytes[8..16].copy_from_slice(&generation.to_be_bytes());
                bytes[40..48].copy_from_slice(&doc_id.to_be_bytes());
            }
            Self::Key { digest, doc_id } => {
                bytes[0] = 2;
                bytes[8..40].copy_from_slice(&digest);
                bytes[40..48].copy_from_slice(&doc_id.to_be_bytes());
            }
        }
        bytes
    }

    pub(in crate::row_locks) fn decode(bytes: &[u8]) -> Option<Self> {
        let doc_id = u64::from_be_bytes(bytes.get(40..48)?.try_into().ok()?);
        if bytes.get(1..8)? != [0; 7] {
            return None;
        }
        match bytes[0] {
            1 if bytes.get(16..40)? == [0; 24] => {
                let generation = u64::from_be_bytes(bytes.get(8..16)?.try_into().ok()?);
                (generation != 0).then_some(Self::Relation { generation, doc_id })
            }
            2 => Some(Self::Key {
                digest: bytes.get(8..40)?.try_into().ok()?,
                doc_id,
            }),
            _ => None,
        }
    }

    pub(in crate::row_locks) fn hash(self) -> u64 {
        super::stable_hash(&[&self.encode()])
    }

    pub(in crate::row_locks) fn key(relation: &[u8], doc_id: u64) -> Option<Self> {
        let digest = relation
            .strip_prefix(b"\xffkey-reservation")?
            .try_into()
            .ok()?;
        Some(Self::Key { digest, doc_id })
    }
}
