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
use crate::mvcc::key::RecordKey;
use crate::mvcc::{
    commit::{PreparedLookup, RecordWriteKind},
    resolution::ResolutionMode,
    CommitSequence, CommittedRecordSnapshot, PreparedRecordCommit, PreparedRecordWrite,
    VersionError, VersionResult,
};
use crate::read_control::StorageReadControl;
use std::cell::RefCell;
use uqa_core::memory::BudgetedVec;

mod writes;
use writes::ResolvedChanges;
mod stream;

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
        validated_marker: RefCell::new(None),
        validated_fence: RefCell::new(None),
    };
    stream::apply(original, &writes, &resolver)?;
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
    validated_marker: RefCell<Option<RecordKey>>,
    validated_fence: RefCell<Option<BudgetedVec<u8>>>,
}

struct Source {
    kind: Kind,
    fence: BudgetedVec<u8>,
    marker: BudgetedVec<u8>,
    marker_kind: RecordWriteKind,
    structural: bool,
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

    /// Select only immutable prepared metadata; this never reenters a committed provider.
    fn source(
        &self,
        write: &PreparedRecordWrite,
        writes: &PreparedLookup<'_>,
    ) -> VersionResult<Source> {
        let control = self.control;
        let kind = self.layout.kind(write.key(), control)?;
        let fence = self
            .layout
            .related_key(write.key(), Related::Structure, control)?;
        let marker = self
            .layout
            .related_key(write.key(), Related::Format, control)?;
        let source = writes
            .metadata(&marker, control)?
            .ok_or(VersionError::InvalidEncoding(
                "occurrence changes lack their source marker",
            ))?;
        let structural =
            writes.contains(&fence, control)? || source.kind() == RecordWriteKind::Canonical;
        Ok(Source {
            kind,
            fence,
            marker,
            marker_kind: source.kind(),
            structural,
        })
    }

    fn merge_write(
        &self,
        mutation: usize,
        write: &PreparedRecordWrite,
        writes: &PreparedLookup<'_>,
        source: Source,
    ) -> VersionResult<()> {
        let control = self.control;
        let changes = self.changes;
        let base = self.base;
        let current = self.current;
        let Source {
            kind,
            fence,
            marker,
            marker_kind,
            ..
        } = source;
        self.validate_marker(&marker, marker_kind, writes)?;
        self.validate_fence(mutation, fence)?;
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
                if &*marker == write.key() {
                    self.replace_current(write, write.value())?;
                } else {
                    self.replace(&marker, write.value())?;
                }
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

    fn validate_marker(
        &self,
        marker: &[u8],
        kind: RecordWriteKind,
        writes: &PreparedLookup<'_>,
    ) -> VersionResult<()> {
        if kind != RecordWriteKind::Occurrence {
            return Err(VersionError::InvalidEncoding(
                "invalid occurrence source marker",
            ));
        }
        if self
            .validated_marker
            .borrow()
            .as_ref()
            .is_none_or(|key| key.bytes() != marker)
        {
            let source = writes
                .get(marker, self.control)?
                .expect("the immutable prepared source contains this marker");
            if !self.is_format(marker, source.value())? {
                return Err(VersionError::InvalidEncoding(
                    "invalid occurrence source marker",
                ));
            }
            *self.validated_marker.borrow_mut() = Some(source.shared_key());
        }
        Ok(())
    }

    fn validate_fence(&self, mutation: usize, fence: BudgetedVec<u8>) -> VersionResult<()> {
        if self.base.sequence() != self.current.sequence()
            && self
                .validated_fence
                .borrow()
                .as_ref()
                .is_none_or(|key| **key != *fence)
        {
            let expected = revision(self.base, &fence, self.control)?;
            let actual = revision(self.current, &fence, self.control)?;
            if expected != actual {
                return Err(VersionError::WriteConflict {
                    mutation,
                    expected,
                    actual,
                });
            }
            // Both committed views are fixed for this resolution; only a new fence can have another result.
            *self.validated_fence.borrow_mut() = Some(fence);
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

    pub(super) fn encode_current(
        &self,
        write: &PreparedRecordWrite,
        template: &[u8],
        value: Value<'_>,
    ) -> VersionResult<()> {
        let bytes = self
            .layout
            .encode(write.key(), template, value, self.control)?;
        self.replace_current(write, Some(&bytes))
    }

    /// Replacing the input key preserves order while no related-key effect has
    /// required the merged overlay. Its revision still belongs to the current view.
    pub(super) fn replace_current(
        &self,
        write: &PreparedRecordWrite,
        value: Option<&[u8]>,
    ) -> VersionResult<()> {
        self.changes
            .preserve(self.replacement(write.key(), value)?, self.control)
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
        self.changes
            .replace(self.replacement(key, value)?, self.control)
    }

    fn replacement(&self, key: &[u8], value: Option<&[u8]>) -> VersionResult<PreparedRecordWrite> {
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
        Ok(write.with_kind(self.mode.kind(kind)))
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
