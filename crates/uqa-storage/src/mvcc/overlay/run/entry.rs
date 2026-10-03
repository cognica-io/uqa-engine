//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The encoding of one spilled change: its key and metadata in the entry file, and the place of its value in the value file.

use uqa_core::memory::MemoryBudget;

use crate::mvcc::commit::RecordWriteKind;
use crate::mvcc::key::RecordKey;
use crate::mvcc::{CommitSequence, PrivateRecordRevision, VersionError, VersionResult};

const EXPECTED: u8 = 1;
const VALUE: u8 = 2;

/// The bytes of a value in a run's value file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::mvcc) struct ValueLocation {
    pub(in crate::mvcc) offset: u64,
    pub(in crate::mvcc) len: u64,
}

/// One change read back from a run, with its key owned and its value, if it has one, still in the value file.
pub(in crate::mvcc) struct RunEntry {
    pub(in crate::mvcc) key: RecordKey,
    pub(in crate::mvcc) expected: Option<CommitSequence>,
    pub(in crate::mvcc) kind: RecordWriteKind,
    pub(in crate::mvcc) identity: PrivateRecordRevision,
    pub(in crate::mvcc) value: Option<ValueLocation>,
}

/// An encoded entry, borrowing its key from the block that holds it.
pub(super) struct RawEntry<'a> {
    pub(super) key: &'a [u8],
    pub(super) expected: Option<CommitSequence>,
    pub(super) kind: RecordWriteKind,
    pub(super) identity: PrivateRecordRevision,
    pub(super) value: Option<ValueLocation>,
}

impl RawEntry<'_> {
    pub(super) fn owned(&self, memory: &MemoryBudget) -> VersionResult<RunEntry> {
        Ok(RunEntry {
            key: RecordKey::new(self.key, memory)?,
            expected: self.expected,
            kind: self.kind,
            identity: self.identity,
            value: self.value,
        })
    }
}

/// The encoded length of an entry whose key has `key_len` bytes.
pub(super) const fn encoded_len(key_len: usize, expected: bool, value: bool) -> usize {
    4 + key_len + 1 + if expected { 8 } else { 0 } + 1 + 8 + if value { 16 } else { 0 }
}

/// Append the encoding of an entry: the key length and key, a flag byte, the expected revision when there is one, the write kind, the private revision and, for a live value, its place in the value file.
pub(super) fn encode(
    output: &mut Vec<u8>,
    key: &[u8],
    expected: Option<CommitSequence>,
    kind: RecordWriteKind,
    identity: PrivateRecordRevision,
    value: Option<ValueLocation>,
) -> VersionResult<()> {
    let key_len = u32::try_from(key.len())
        .map_err(|_| VersionError::InvalidEncoding("spilled record key exceeds 4 GiB"))?;
    output.extend_from_slice(&key_len.to_le_bytes());
    output.extend_from_slice(key);
    let flags =
        if expected.is_some() { EXPECTED } else { 0 } | if value.is_some() { VALUE } else { 0 };
    output.push(flags);
    if let Some(expected) = expected {
        output.extend_from_slice(&expected.as_u64().to_le_bytes());
    }
    output.push(kind.code());
    output.extend_from_slice(&identity.as_u64().to_le_bytes());
    if let Some(value) = value {
        output.extend_from_slice(&value.offset.to_le_bytes());
        output.extend_from_slice(&value.len.to_le_bytes());
    }
    Ok(())
}

/// Decode the entry at `*position` and advance past it.
pub(super) fn decode<'a>(bytes: &'a [u8], position: &mut usize) -> VersionResult<RawEntry<'a>> {
    let mut take = |count: usize| -> VersionResult<&'a [u8]> {
        let end = position
            .checked_add(count)
            .filter(|end| *end <= bytes.len())
            .ok_or(VersionError::InvalidEncoding("truncated spilled record"))?;
        let slice = &bytes[*position..end];
        *position = end;
        Ok(slice)
    };
    let key_len = u32::from_le_bytes(take(4)?.try_into().expect("four bytes")) as usize;
    let key = take(key_len)?;
    let flags = take(1)?[0];
    if flags & !(EXPECTED | VALUE) != 0 {
        return Err(VersionError::InvalidEncoding(
            "invalid spilled record flags",
        ));
    }
    let expected = if flags & EXPECTED != 0 {
        Some(CommitSequence::from_u64(u64::from_le_bytes(
            take(8)?.try_into().expect("eight bytes"),
        )))
    } else {
        None
    };
    let kind = RecordWriteKind::from_code(take(1)?[0])?;
    let identity = PrivateRecordRevision::from_u64(u64::from_le_bytes(
        take(8)?.try_into().expect("eight bytes"),
    ))?;
    let value = if flags & VALUE != 0 {
        let offset = u64::from_le_bytes(take(8)?.try_into().expect("eight bytes"));
        let len = u64::from_le_bytes(take(8)?.try_into().expect("eight bytes"));
        Some(ValueLocation { offset, len })
    } else {
        None
    };
    Ok(RawEntry {
        key,
        expected,
        kind,
        identity,
        value,
    })
}
