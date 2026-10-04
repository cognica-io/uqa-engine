//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! What a prepared write changes, without its value.

use crate::mvcc::key::RecordKey;
use crate::mvcc::CommitSequence;

use super::RecordWriteKind;

/// What a write changes, without its value: its key, the revision it expects, its kind and the length of its value, if it has one.
pub struct PreparedWriteMetadata {
    key: RecordKey,
    expected: Option<CommitSequence>,
    kind: RecordWriteKind,
    value_len: Option<u64>,
}

impl PreparedWriteMetadata {
    pub(super) fn new(
        key: RecordKey,
        expected: Option<CommitSequence>,
        kind: RecordWriteKind,
        value_len: Option<u64>,
    ) -> Self {
        Self {
            key,
            expected,
            kind,
            value_len,
        }
    }

    pub fn key(&self) -> &[u8] {
        self.key.bytes()
    }

    pub fn expected(&self) -> Option<CommitSequence> {
        self.expected
    }

    /// Whether the write has a value; a write without one is a deletion.
    pub fn live(&self) -> bool {
        self.value_len.is_some()
    }

    /// The length of the write's value, if it has one.
    pub fn value_len(&self) -> Option<u64> {
        self.value_len
    }

    pub(crate) fn kind(&self) -> RecordWriteKind {
        self.kind
    }
}
