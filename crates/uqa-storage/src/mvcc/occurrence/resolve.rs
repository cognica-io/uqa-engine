//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resolve evaluated occurrence changes against one fresh committed view without replaying analysis or scoring.

use super::{
    OccurrenceRecordKind as Kind, OccurrenceRecordLayout, OccurrenceRecordValue as Value,
    OccurrenceRelatedKey as Related,
};
use crate::mvcc::{
    commit::{PreparedLookup, RecordWriteKind},
    resolution::ResolutionMode,
    CommitSequence, CommittedRecordSnapshot, PreparedRecordCommit, PreparedRecordWrite,
    VersionError, VersionResult,
};
use crate::read_control::StorageReadControl;
use uqa_core::memory::BudgetedVec;

mod writes;
use writes::ResolvedChanges;

pub(in crate::mvcc) fn resolve(
    original: &PreparedRecordCommit,
    base: &dyn CommittedRecordSnapshot,
    current: &dyn CommittedRecordSnapshot,
    layout: &dyn OccurrenceRecordLayout,
    mode: ResolutionMode,
    control: &StorageReadControl,
) -> VersionResult<PreparedRecordCommit> {
    let changes = ResolvedChanges::new(original, control)?;
    let writes = PreparedLookup::new(original, control)?;
    let resolver = Resolver {
        base,
        current,
        layout,
        mode,
        changes: &changes,
        control,
    };
    let mut originals = original.writes();
    let mut mutation = 0;
    while let Some(write) = originals.next(control)? {
        let write = &write;
        match write.kind() {
            RecordWriteKind::Canonical => {
                resolver.validate(mutation, write)?;
                changes.preserve(write.clone(), control)?;
            }
            RecordWriteKind::GraphCache
            | RecordWriteKind::GraphPreview
            | RecordWriteKind::IVFPreview
            | RecordWriteKind::HNSWPreview
            | RecordWriteKind::Marker
            | RecordWriteKind::DiskANNOrigin
            | RecordWriteKind::DiskANNPopulationPreview
            | RecordWriteKind::IdempotentDelete
            | RecordWriteKind::StatisticsMaintenance => {
                changes.preserve(write.clone(), control)?;
            }
            RecordWriteKind::Occurrence | RecordWriteKind::OccurrenceCache => {
                resolver.merge_write(mutation, write, &writes)?;
            }
        }
        mutation += 1;
    }
    Ok(changes
        .finish(control)?
        .retain_graph_effects(original, control)?
        .resolved(original, current.sequence()))
}

pub(super) struct Resolver<'a> {
    pub(super) base: &'a dyn CommittedRecordSnapshot,
    pub(super) current: &'a dyn CommittedRecordSnapshot,
    pub(super) layout: &'a dyn OccurrenceRecordLayout,
    mode: ResolutionMode,
    changes: &'a ResolvedChanges,
    pub(super) control: &'a StorageReadControl,
}

pub(super) fn revision(
    snapshot: &dyn CommittedRecordSnapshot,
    key: &[u8],
    control: &StorageReadControl,
) -> VersionResult<Option<CommitSequence>> {
    Ok(snapshot
        .metadata(key, control)?
        .and_then(|row| row.revision))
}

impl Resolver<'_> {
    pub(super) fn preserve(&self, write: &PreparedRecordWrite) -> VersionResult<()> {
        self.changes.replace(
            write.clone().with_kind(self.mode.kind(write.kind())),
            self.control,
        )
    }

    fn merge_write(
        &self,
        mutation: usize,
        write: &PreparedRecordWrite,
        writes: &PreparedLookup<'_>,
    ) -> VersionResult<()> {
        let control = self.control;
        let changes = self.changes;
        let base = self.base;
        let current = self.current;

        let kind = self.layout.kind(write.key(), control)?;
        let fence = self
            .layout
            .related_key(write.key(), Related::Structure, control)?;
        let marker = self
            .layout
            .related_key(write.key(), Related::Format, control)?;
        let source = writes
            .get(&marker, control)?
            .ok_or(VersionError::InvalidEncoding(
                "occurrence changes lack their source marker",
            ))?;
        if writes.contains(&fence, control)? || source.kind() == RecordWriteKind::Canonical {
            self.validate(mutation, write)?;
            changes.preserve(write.clone().with_kind(RecordWriteKind::Canonical), control)?;
            return Ok(());
        }
        if source.kind() != RecordWriteKind::Occurrence
            || !self.is_format(&marker, source.value())?
        {
            return Err(VersionError::InvalidEncoding(
                "invalid occurrence source marker",
            ));
        }
        if base.sequence() != current.sequence() {
            let expected = revision(base, &fence, control)?;
            let actual = revision(current, &fence, control)?;
            if expected != actual {
                return Err(VersionError::WriteConflict {
                    mutation,
                    expected,
                    actual,
                });
            }
        }
        match (write.kind(), kind) {
            (RecordWriteKind::OccurrenceCache, Kind::Cache) => {}
            (RecordWriteKind::Occurrence, Kind::Format) => {
                for snapshot in [base, current] {
                    let row = snapshot.get(&marker, control)?;
                    let value = bytes(row.as_ref());
                    if value.is_some() && !self.is_format(&marker, value)? {
                        return Err(VersionError::InvalidEncoding(
                            "occurrence format changed during source replacement",
                        ));
                    }
                }
                self.replace(&marker, write.value())?;
                self.invalidate(&marker)?;
            }
            (RecordWriteKind::Occurrence, Kind::Score(_) | Kind::Positions) => {
                let peer_key = self.layout.related_key(
                    write.key(),
                    if kind == Kind::Positions {
                        Related::Score
                    } else {
                        Related::Positions
                    },
                    control,
                )?;
                let paired =
                    writes
                        .get(&peer_key, control)?
                        .ok_or(VersionError::InvalidEncoding(
                            "unpaired occurrence cluster replacement",
                        ))?;
                if paired.kind() != RecordWriteKind::Occurrence {
                    self.validate(mutation, write)?;
                    changes
                        .preserve(write.clone().with_kind(RecordWriteKind::Canonical), control)?;
                } else if let Kind::Score(cluster) = kind {
                    self.merge_cluster(mutation, write, Some(&paired), cluster)?;
                }
            }
            (RecordWriteKind::Occurrence, Kind::Cluster(cluster)) => {
                self.merge_cluster(mutation, write, None, cluster)?;
            }
            (RecordWriteKind::Occurrence, Kind::Statistics) => {
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

    fn is_format(&self, key: &[u8], value: Option<&[u8]>) -> VersionResult<bool> {
        Ok(match value {
            Some(value) => matches!(
                self.layout.decode(key, value, self.control)?,
                Value::Format(b"occurrences-v2")
            ),
            None => false,
        })
    }

    pub(super) fn encode_replace(
        &self,
        key: &[u8],
        template: &[u8],
        value: Value<'_>,
    ) -> VersionResult<()> {
        let bytes = self.layout.encode(key, template, value, self.control)?;
        self.replace(key, Some(&bytes))
    }

    fn validate(&self, mutation: usize, write: &PreparedRecordWrite) -> VersionResult<()> {
        // Publication preserves these canonical preconditions for the physical writer's atomic validation. Command refresh must check them before exposing a new private view.
        if self.mode == ResolutionMode::Publication {
            return Ok(());
        }
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
    pub(super) fn replace(&self, key: &[u8], value: Option<&[u8]>) -> VersionResult<()> {
        let kind = if self.layout.kind(key, self.control)? == Kind::Cache {
            RecordWriteKind::OccurrenceCache
        } else {
            RecordWriteKind::Occurrence
        };
        let write = PreparedRecordWrite::copy_bytes(
            key,
            revision(self.current, key, self.control)?,
            value,
            self.control,
        )?;
        self.changes
            .replace(write.with_kind(self.mode.kind(kind)), self.control)
    }
    fn invalidate(&self, marker: &[u8]) -> VersionResult<()> {
        for kind in [Related::Skips, Related::BlockMax] {
            let prefix = self.layout.related_key(marker, kind, self.control)?;
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

pub(super) fn bytes(
    record: Option<&crate::mvcc::RecordVersion<crate::mvcc::SharedRecordValue>>,
) -> Option<&[u8]> {
    record
        .and_then(|record| record.value())
        .map(|value| &***value)
}
