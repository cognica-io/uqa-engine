//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resolve only derived graph state against a fresh committed view; canonical writes keep their original preconditions.

use std::sync::Arc;

use uqa_core::memory::BudgetedVec;

use crate::mvcc::commit::RecordWriteKind;
use crate::mvcc::{
    CommitSequence, CommittedRecordSnapshot, DatabaseId, MergedRecordSnapshot,
    PreparedRecordCommit, PrivateRecordChanges, RecordWrite, ScannedVisibleRecord, VersionError,
    VersionResult,
};
use crate::read_control::StorageReadControl;

use super::{GraphMutation, GraphRecordKey, GraphRecordLayout};

struct Resolver<'a> {
    committed: Arc<dyn CommittedRecordSnapshot>,
    final_view: MergedRecordSnapshot,
    private_preview: MergedRecordSnapshot,
    changes: PrivateRecordChanges,
    layout: &'a dyn GraphRecordLayout,
    database: DatabaseId,
    control: &'a StorageReadControl,
}

pub(in crate::mvcc) fn resolve(
    original: &PreparedRecordCommit,
    committed: Arc<dyn CommittedRecordSnapshot>,
    layout: &dyn GraphRecordLayout,
    database: DatabaseId,
    control: &StorageReadControl,
) -> VersionResult<PreparedRecordCommit> {
    let effects = original
        .graph
        .as_ref()
        .expect("graph effects were requested");
    let changes = PrivateRecordChanges::new(control.memory());
    let preview = PrivateRecordChanges::new(control.memory());
    preview.apply_owned(original.records(), control)?;
    let mut writes = BudgetedVec::new(control.memory());
    writes.reserve(original.records().len())?;
    for (mutation, write) in original.records().iter().enumerate() {
        control.cancellation().check()?;
        let actual = committed
            .metadata(write.key(), control)?
            .and_then(|head| head.revision);
        if write.kind() == RecordWriteKind::Canonical {
            if write.expected() != actual {
                return Err(VersionError::WriteConflict {
                    mutation,
                    expected: write.expected(),
                    actual,
                });
            }
            writes.push(write.clone())?;
        } else {
            if !layout.is_validity_key(write.key())? {
                return Err(VersionError::InvalidEncoding(
                    "graph cache write targets a canonical record",
                ));
            }
            if write.kind() == RecordWriteKind::GraphCache {
                writes.push(
                    write
                        .clone()
                        .rebase(actual)
                        .with_kind(RecordWriteKind::Canonical),
                )?;
            }
        }
    }
    changes.apply_owned(&writes, control)?;
    drop(writes);
    let resolver = Resolver {
        final_view: MergedRecordSnapshot::new(committed.clone(), changes.snapshot()?),
        private_preview: MergedRecordSnapshot::new(committed.clone(), preview.snapshot()?),
        committed,
        changes,
        layout,
        database,
        control,
    };
    for operation in effects.operations.iter() {
        control.cancellation().check()?;
        match operation.borrowed() {
            GraphMutation::InvalidateGraph(graph) => resolver.invalidate_graph(graph)?,
            GraphMutation::InvalidatePath(index) => {
                resolver.invalidate_key(&resolver.key(GraphRecordKey::PathValidity(index))?)?;
            }
            GraphMutation::InvalidateEntity(kind, id) => {
                let prefix = resolver.key(GraphRecordKey::EntityMemberships(kind, id))?;
                visit(&resolver.final_view, &prefix, control, |entry| {
                    if entry.record.value().is_some() {
                        let graph = layout.membership_graph(&entry.key, control)?;
                        let graph = std::str::from_utf8(&graph).map_err(|_| {
                            VersionError::InvalidEncoding("graph membership name is not UTF-8")
                        })?;
                        resolver.invalidate_graph(graph)?;
                    }
                    Ok(true)
                })?;
            }
            GraphMutation::PublishPath {
                index,
                graph,
                definition,
            } => {
                resolver.publish_path(index, graph, definition, effects.base)?;
            }
        }
    }
    Ok(resolver
        .changes
        .prepare(control)?
        .resolved(original, resolver.committed.sequence()))
}

impl Resolver<'_> {
    fn key(&self, key: GraphRecordKey<'_>) -> VersionResult<BudgetedVec<u8>> {
        self.layout.key(self.database, key, self.control)
    }

    fn replace(&self, key: &[u8], value: Option<&[u8]>) -> VersionResult<()> {
        let expected = self
            .committed
            .metadata(key, self.control)?
            .and_then(|record| record.revision);
        self.changes.apply(
            &[RecordWrite {
                key,
                expected,
                value,
            }],
            self.control,
        )
    }

    fn invalidate_graph(&self, graph: &str) -> VersionResult<()> {
        let prefix = self.key(GraphRecordKey::GraphPaths(graph))?;
        visit(&self.final_view, &prefix, self.control, |entry| {
            let Some(value) = entry.record.value() else {
                return Ok(true);
            };
            let Some(key) = self.layout.path_validity_key(
                &self.final_view,
                self.database,
                graph,
                &entry.key,
                value,
                self.control,
            )?
            else {
                return Ok(true);
            };
            self.invalidate_key(&key)?;
            Ok(true)
        })
    }

    fn invalidate_key(&self, key: &[u8]) -> VersionResult<()> {
        let view = MergedRecordSnapshot::new(self.committed.clone(), self.changes.snapshot()?);
        if let Some(row) = view.get(key, self.control)? {
            if let Some(value) = row.value() {
                let invalid = self.layout.invalidate(key, value, self.control)?;
                self.replace(key, invalid.as_deref())?;
            }
        }
        Ok(())
    }

    fn publish_path(
        &self,
        index: &str,
        graph: &str,
        definition: &str,
        base: CommitSequence,
    ) -> VersionResult<()> {
        let key = self.key(GraphRecordKey::PathValidity(index))?;
        let Some(row) = self.private_preview.get(&key, self.control)? else {
            return Ok(());
        };
        let Some(value) = row.value() else {
            return Ok(());
        };
        if !self.layout.is_published(
            &self.final_view,
            &key,
            value,
            graph,
            definition,
            self.control,
        )? {
            return Ok(());
        }
        let definition_key = self.key(GraphRecordKey::PathDefinition(index))?;
        let current = self.final_view.get(&definition_key, self.control)?;
        let definition_matches = match current.as_ref().and_then(|record| record.value()) {
            Some(value) => {
                self.layout
                    .definition_matches(&definition_key, value, definition, self.control)?
            }
            None => false,
        };
        if definition_matches
            && !self.changed(&definition_key, base)?
            && !self.graph_changed(graph, base)?
        {
            self.replace(&key, Some(value))
        } else {
            let invalid = self.layout.invalidate(&key, value, self.control)?;
            self.replace(&key, invalid.as_deref())
        }
    }

    fn changed(&self, key: &[u8], base: CommitSequence) -> VersionResult<bool> {
        Ok(self
            .committed
            .metadata(key, self.control)?
            .and_then(|record| record.revision)
            .is_some_and(|revision| revision > base))
    }

    fn graph_changed(&self, graph: &str, base: CommitSequence) -> VersionResult<bool> {
        for input in [
            GraphRecordKey::GraphName(graph),
            GraphRecordKey::LabelRegistry(graph),
        ] {
            if self.changed(&self.key(input)?, base)? {
                return Ok(true);
            }
        }
        let prefix = self.key(GraphRecordKey::GraphMemberships(graph))?;
        let mut changed = false;
        visit(&self.final_view, &prefix, self.control, |entry| {
            if self.changed(&entry.key, base)? {
                changed = true;
            } else if entry.record.value().is_some() {
                let (kind, id) = self.layout.membership_entity(&entry.key, self.control)?;
                changed = self.changed(&self.key(GraphRecordKey::Entity(kind, id))?, base)?;
            }
            Ok(!changed)
        })?;
        Ok(changed)
    }
}

/// Each page releases provider I/O before a consumer probes another record or stages an effect. The cursor includes tombstones so a fully deleted page cannot end a scan early.
fn visit(
    view: &MergedRecordSnapshot,
    prefix: &[u8],
    control: &StorageReadControl,
    mut visitor: impl FnMut(&ScannedVisibleRecord) -> VersionResult<bool>,
) -> VersionResult<()> {
    let mut after = BudgetedVec::new(control.memory());
    loop {
        let page = view.scan(prefix, (!after.is_empty()).then_some(&*after), 64, control)?;
        let Some(last) = page.last() else {
            return Ok(());
        };
        after.clear();
        after.extend_from_slice(&last.key)?;
        for entry in page.iter() {
            control.cancellation().check()?;
            if !visitor(entry)? {
                return Ok(());
            }
        }
    }
}
