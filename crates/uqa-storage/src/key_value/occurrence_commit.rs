//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Merge evaluated common-format occurrence records without repeating analysis or scoring.

mod clusters;
mod statistics;

use super::occurrence_format::{OccurrenceAddress, OccurrenceProjection};
use super::occurrence_keys::{
    self as keys,
    encoding::{controlled, Part},
};
use crate::mvcc::{
    commit::RecordWriteKind, CommitSequence, CommittedRecordSnapshot, PreparedRecordCommit,
    PreparedRecordWrite, PrivateRecordChanges, RecordWrite, VersionError, VersionResult,
};
use crate::read_control::StorageReadControl;
use std::collections::BTreeMap;
use uqa_core::memory::{BudgetedVec, MemoryError};

// Keep guards outside the data namespace so drop/recreation cannot reuse a structural boundary.
const GUARDS: u8 = b'z';

pub(crate) fn guard(
    table: &str,
    document: Option<u64>,
    control: &StorageReadControl,
) -> VersionResult<BudgetedVec<u8>> {
    Ok(match document {
        Some(document) => controlled(
            table,
            GUARDS,
            Some(b'd'),
            &[Part::Number(document)],
            control,
        )?,
        None => controlled(table, GUARDS, Some(b's'), &[], control)?,
    })
}

pub(crate) fn format(table: &str, control: &StorageReadControl) -> VersionResult<BudgetedVec<u8>> {
    Ok(controlled(
        table,
        super::TAG_OCCURRENCE_INDEX,
        Some(keys::FORMAT),
        &[],
        control,
    )?)
}

pub(crate) fn resolve(
    original: &PreparedRecordCommit,
    base: &dyn CommittedRecordSnapshot,
    current: &dyn CommittedRecordSnapshot,
    control: &StorageReadControl,
) -> VersionResult<PreparedRecordCommit> {
    let changes = PrivateRecordChanges::new(control.memory());
    let size = original
        .records()
        .len()
        .checked_mul(std::mem::size_of::<(&[u8], &PreparedRecordWrite)>())
        .ok_or(MemoryError::SizeOverflow)?;
    // Charge logical entries; allocator-specific tree-node bookkeeping follows the other record maps.
    let _lookup_memory = control.memory().reserve(size)?;
    let mut writes = BTreeMap::new();
    for write in original.records() {
        control.cancellation().check()?;
        writes.insert(write.key(), write);
    }
    let resolver = Resolver {
        base,
        current,
        changes: &changes,
        control,
    };
    for (mutation, write) in original.records().iter().enumerate() {
        control.cancellation().check()?;
        match write.kind() {
            RecordWriteKind::Canonical => {
                resolver.validate(mutation, write)?;
                changes.apply_owned(std::slice::from_ref(write), control)?;
            }
            RecordWriteKind::GraphCache | RecordWriteKind::GraphPreview => {
                changes.apply_owned(std::slice::from_ref(write), control)?;
            }
            RecordWriteKind::Occurrence | RecordWriteKind::OccurrenceCache => {
                resolver.merge_write(mutation, write, &writes)?;
            }
        }
    }
    Ok(changes
        .prepare(control)?
        .retain_graph_effects(original, control)?
        .resolved(original, current.sequence()))
}

struct Resolver<'a> {
    base: &'a dyn CommittedRecordSnapshot,
    current: &'a dyn CommittedRecordSnapshot,
    changes: &'a PrivateRecordChanges,
    control: &'a StorageReadControl,
}

fn revision(
    snapshot: &dyn CommittedRecordSnapshot,
    key: &[u8],
    control: &StorageReadControl,
) -> VersionResult<Option<CommitSequence>> {
    Ok(snapshot
        .metadata(key, control)?
        .and_then(|row| row.revision))
}

impl Resolver<'_> {
    fn merge_write(
        &self,
        mutation: usize,
        write: &PreparedRecordWrite,
        writes: &BTreeMap<&[u8], &PreparedRecordWrite>,
    ) -> VersionResult<()> {
        let control = self.control;
        let changes = self.changes;
        let base = self.base;
        let current = self.current;

        let address = OccurrenceAddress::decode(write.key())?;
        if !address.complete() {
            return Err(VersionError::InvalidEncoding(
                "incomplete occurrence replacement",
            ));
        }
        let fence = guard(address.table, None, control)?;
        let marker = format(address.table, control)?;
        let source = writes.get(&*marker).ok_or(VersionError::InvalidEncoding(
            "occurrence changes lack their source marker",
        ))?;
        if writes.contains_key(&*fence) || source.kind() == RecordWriteKind::Canonical {
            self.validate(mutation, write)?;
            changes.apply_owned(
                &[write.clone().with_kind(RecordWriteKind::Canonical)],
                control,
            )?;
            return Ok(());
        }
        if source.kind() != RecordWriteKind::Occurrence || source.value() != Some(keys::FORMAT_NAME)
        {
            return Err(VersionError::InvalidEncoding(
                "invalid occurrence source marker",
            ));
        }
        let expected = revision(base, &fence, control)?;
        let actual = revision(current, &fence, control)?;
        if expected != actual {
            return Err(VersionError::WriteConflict {
                mutation,
                expected,
                actual,
            });
        }
        match (write.kind(), address.projection) {
            (
                RecordWriteKind::OccurrenceCache,
                Some(OccurrenceProjection::Skip | OccurrenceProjection::BlockMax),
            ) => {}
            (RecordWriteKind::Occurrence, Some(OccurrenceProjection::Format)) => {
                for snapshot in [base, current] {
                    if snapshot
                        .get(&marker, control)?
                        .as_ref()
                        .and_then(|row| row.value())
                        .is_some_and(|value| &***value != keys::FORMAT_NAME)
                    {
                        return Err(VersionError::InvalidEncoding(
                            "occurrence format changed during source replacement",
                        ));
                    }
                }
                self.replace(&marker, write.value())?;
                self.invalidate(address.table)?;
            }
            (
                RecordWriteKind::Occurrence,
                Some(OccurrenceProjection::Score | OccurrenceProjection::Positions),
            ) => {
                let mut peer = address;
                peer.projection =
                    Some(if address.projection == Some(OccurrenceProjection::Score) {
                        OccurrenceProjection::Positions
                    } else {
                        OccurrenceProjection::Score
                    });
                let peer_key = peer.encode(control)?;
                let paired = writes.get(&*peer_key).ok_or(VersionError::InvalidEncoding(
                    "unpaired occurrence cluster replacement",
                ))?;
                if paired.kind() != RecordWriteKind::Occurrence {
                    self.validate(mutation, write)?;
                    changes.apply_owned(
                        &[write.clone().with_kind(RecordWriteKind::Canonical)],
                        control,
                    )?;
                } else if address.projection == Some(OccurrenceProjection::Score) {
                    self.merge_cluster(
                        mutation,
                        write,
                        paired,
                        address.cluster.expect("complete cluster"),
                    )?;
                }
            }
            (RecordWriteKind::Occurrence, Some(OccurrenceProjection::Field)) => {
                self.merge_statistics(mutation, write)?;
            }
            _ => {
                return Err(VersionError::InvalidEncoding(
                    "occurrence merge targets a canonical record",
                ))
            }
        }
        Ok(())
    }

    fn validate(&self, mutation: usize, write: &PreparedRecordWrite) -> VersionResult<()> {
        let actual = revision(self.current, write.key(), self.control)?;
        if actual != write.expected() {
            return Err(VersionError::WriteConflict {
                mutation,
                expected: write.expected(),
                actual,
            });
        }
        Ok(())
    }
    fn replace(&self, key: &[u8], value: Option<&[u8]>) -> VersionResult<()> {
        self.changes.apply(
            &[RecordWrite {
                key,
                expected: revision(self.current, key, self.control)?,
                value,
            }],
            self.control,
        )
    }
    fn invalidate(&self, table: &str) -> VersionResult<()> {
        for kind in [keys::SKIP, keys::BLOCK_MAX] {
            let prefix = controlled(
                table,
                super::TAG_OCCURRENCE_INDEX,
                Some(kind),
                &[],
                self.control,
            )?;
            let mut after = BudgetedVec::new(self.control.memory());
            loop {
                let page = self.current.scan(
                    &prefix,
                    (!after.is_empty()).then_some(&*after),
                    64,
                    self.control,
                )?;
                let Some(last) = page.last() else {
                    break;
                };
                after.clear();
                after.extend_from_slice(&last.key)?;
                for row in page.iter() {
                    self.control.cancellation().check()?;
                    if row.version.value().is_some() {
                        self.replace(&row.key, None)?;
                    }
                }
            }
        }
        Ok(())
    }
}

fn bytes(
    record: Option<&crate::mvcc::RecordVersion<crate::mvcc::SharedRecordValue>>,
) -> Option<&[u8]> {
    record
        .and_then(|record| record.value())
        .map(|value| &***value)
}
