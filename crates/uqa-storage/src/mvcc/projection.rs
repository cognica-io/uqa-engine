//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Value and key-only projections share the same private/committed merge algorithm.

use super::{
    BorrowedRecord, CommittedRecordSnapshot, PreparedRecordWrite, RecordMetadata, VersionResult,
};
use crate::read_control::StorageReadControl;

pub(super) type Visitor<'a, P> =
    dyn for<'record> FnMut(&[u8], <P as Projection>::Record<'record>) -> VersionResult<bool> + 'a;

pub(super) trait Projection: 'static {
    type Record<'a>: Copy;
    fn private(write: &PreparedRecordWrite) -> Self::Record<'_>;
    fn visit(
        committed: &dyn CommittedRecordSnapshot,
        prefix: &[u8],
        after: Option<&[u8]>,
        control: &StorageReadControl,
        visitor: &mut Visitor<'_, Self>,
    ) -> VersionResult<()>;
}

pub(super) struct Values;
impl Projection for Values {
    type Record<'a> = BorrowedRecord<'a>;
    fn private(write: &PreparedRecordWrite) -> BorrowedRecord<'_> {
        BorrowedRecord {
            revision: write.expected(),
            value: write.value(),
        }
    }
    fn visit(
        committed: &dyn CommittedRecordSnapshot,
        prefix: &[u8],
        after: Option<&[u8]>,
        control: &StorageReadControl,
        visitor: &mut Visitor<'_, Self>,
    ) -> VersionResult<()> {
        committed.visit_prefix(prefix, after, usize::MAX, control, visitor)
    }
}

pub(super) struct Keys;
impl Projection for Keys {
    type Record<'a> = RecordMetadata;
    fn private(write: &PreparedRecordWrite) -> RecordMetadata {
        RecordMetadata {
            revision: write.expected(),
            live: write.value().is_some(),
        }
    }
    fn visit(
        committed: &dyn CommittedRecordSnapshot,
        prefix: &[u8],
        after: Option<&[u8]>,
        control: &StorageReadControl,
        visitor: &mut Visitor<'_, Self>,
    ) -> VersionResult<()> {
        committed.visit_keys(prefix, after, usize::MAX, control, visitor)
    }
}
