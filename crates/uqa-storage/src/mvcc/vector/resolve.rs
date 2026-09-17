//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Merge only evaluated vector inputs; canonical replacements keep their original preconditions.

mod canonical;

use super::{layout::Layout, IndexKind, Key, Mutation};
use crate::mvcc::{
    commit::RecordWriteKind, resolution::ResolutionMode, CommittedRecordSnapshot,
    PreparedRecordCommit, PreparedRecordWrite, PrivateRecordChanges, RecordWrite, VersionError,
    VersionResult,
};
use crate::read_control::StorageReadControl;
use std::collections::BTreeMap;
use uqa_core::memory::{BudgetedVec, MemoryError};

struct Scope<'a> {
    layout: Layout<'a>,
    kind: IndexKind,
    header: &'a PreparedRecordWrite,
    operations: BudgetedVec<Mutation<'a>>,
    rebase: bool,
}

pub(in crate::mvcc) fn resolve(
    original: &PreparedRecordCommit,
    base: &dyn CommittedRecordSnapshot,
    current: &dyn CommittedRecordSnapshot,
    persistence: &dyn crate::mvcc::VersionedPersistence,
    mode: ResolutionMode,
    control: &StorageReadControl,
) -> VersionResult<PreparedRecordCommit> {
    let effects = original.vector.as_ref().expect("requested vector effects");
    let lookup_bytes = original
        .records()
        .len()
        .checked_mul(size_of::<(&[u8], &PreparedRecordWrite)>())
        .and_then(|n| {
            effects
                .operations
                .len()
                .checked_mul(size_of::<((IndexKind, &[u8]), Scope<'_>)>())
                .and_then(|m| n.checked_add(m))
        })
        .ok_or(MemoryError::SizeOverflow)?;
    let _lookups = control.memory().reserve(lookup_bytes)?;
    let mut writes = BTreeMap::new();
    for (position, write) in original.records().iter().enumerate() {
        control.cancellation().check()?;
        writes.insert(write.key(), write);
        if write.kind() == RecordWriteKind::Canonical {
            validate(current, position, write, control)?;
        }
    }
    let mut scopes = BTreeMap::<(IndexKind, &[u8]), Scope<'_>>::new();
    for operation in effects.operations.iter() {
        control.cancellation().check()?;
        let key = operation.metadata.bytes();
        let scope = if let std::collections::btree_map::Entry::Vacant(entry) =
            scopes.entry((operation.kind, key))
        {
            let header = *writes.get(key).ok_or(VersionError::InvalidEncoding(
                "vector input lacks a metadata replacement",
            ))?;
            entry.insert(Scope {
                layout: operation.kind.layout(persistence)?,
                kind: operation.kind,
                header,
                operations: BudgetedVec::new(control.memory()),
                rebase: false,
            })
        } else {
            scopes
                .get_mut(&(operation.kind, key))
                .expect("existing scope")
        };
        scope.operations.push(operation.borrowed())?;
    }
    for ((_, key), scope) in &mut scopes {
        validate_scope(key, scope, &writes, base, current, control)?;
    }
    let changes = PrivateRecordChanges::new(control.memory());
    for (position, write) in original.records().iter().enumerate() {
        control.cancellation().check()?;
        let Some(kind) = IndexKind::from_preview(write.kind()) else {
            changes.apply_owned(std::slice::from_ref(write), control)?;
            continue;
        };
        let layout = kind.layout(persistence)?;
        let owner =
            layout
                .metadata_key(write.key(), control)?
                .ok_or(VersionError::InvalidEncoding(
                    "vector preview targets a canonical record",
                ))?;
        let scope = scopes
            .get(&(kind, &*owner))
            .ok_or(VersionError::InvalidEncoding(
                "vector preview lacks evaluated document input",
            ))?;
        if !scope.rebase {
            validate(current, position, write, control)?;
            changes.apply_owned(&[write.clone().with_kind(mode.kind(write.kind()))], control)?;
        }
    }
    for ((_, key), scope) in scopes.iter().filter(|(_, scope)| scope.rebase) {
        scope.merge(key, &changes, current, mode, control)?;
    }
    Ok(changes
        .prepare(control)?
        .retain_graph_effects(original, control)?
        .resolved(original, current.sequence()))
}

impl Scope<'_> {
    fn merge(
        &self,
        key: &[u8],
        changes: &PrivateRecordChanges,
        current: &dyn CommittedRecordSnapshot,
        mode: ResolutionMode,
        control: &StorageReadControl,
    ) -> VersionResult<()> {
        if mode == ResolutionMode::Publication {
            return self
                .layout
                .merge(key, &self.operations, changes, current, control);
        }
        let merged = PrivateRecordChanges::new(control.memory());
        self.layout
            .merge(key, &self.operations, &merged, current, control)?;
        for write in merged.prepare(control)?.records() {
            changes.apply_owned(
                &[write.clone().with_kind(mode.kind(self.kind.preview()))],
                control,
            )?;
        }
        Ok(())
    }
}

fn validate_scope(
    key: &[u8],
    scope: &mut Scope<'_>,
    writes: &BTreeMap<&[u8], &PreparedRecordWrite>,
    base: &dyn CommittedRecordSnapshot,
    current: &dyn CommittedRecordSnapshot,
    control: &StorageReadControl,
) -> VersionResult<()> {
    let layout = scope.layout;
    if scope.header.kind() != scope.kind.preview() {
        return Ok(());
    }
    let guard = layout.key(key, Key::Structure, control)?;
    let expected = revision(base, &guard, control)?;
    let actual = revision(current, &guard, control)?;
    if expected != actual {
        return Err(VersionError::WriteConflict {
            mutation: 0,
            expected,
            actual,
        });
    }
    if writes.contains_key(&*guard) {
        return Ok(());
    }
    for write in writes
        .values()
        .filter(|write| write.kind() == RecordWriteKind::Canonical)
    {
        control.cancellation().check()?;
        if layout.metadata_key(write.key(), control)?.as_deref() == Some(key) {
            return Err(VersionError::InvalidEncoding(
                "vector input mixes with unjournaled derived records",
            ));
        }
    }
    let base_row = base.get(key, control)?;
    let latest_row = current.get(key, control)?;
    let old = bytes(base_row.as_ref()).ok_or(VersionError::InvalidEncoding(
        "vector input has no committed definition",
    ))?;
    let Some(latest) = bytes(latest_row.as_ref()) else {
        return Err(VersionError::WriteConflict {
            mutation: 0,
            expected: scope.header.expected(),
            actual: revision(current, key, control)?,
        });
    };
    let before = layout.header(key, old, control)?;
    let now = layout.header(key, latest, control)?;
    let evaluated = layout.header(
        key,
        scope.header.value().ok_or(VersionError::InvalidEncoding(
            "vector preview removed its definition",
        ))?,
        control,
    )?;
    if !before.same_definition(now) || !before.same_definition(evaluated) {
        return Err(VersionError::WriteConflict {
            mutation: 0,
            expected: scope.header.expected(),
            actual: revision(current, key, control)?,
        });
    }
    if before.revision.is_some_and(|revision| {
        revision.checked_add(scope.operations.len() as u64) != evaluated.revision
    }) {
        return Err(VersionError::InvalidEncoding(
            "vector preview does not match its ordered input journal",
        ));
    }
    canonical::validate(key, &scope.operations, writes, base, layout, control)?;
    scope.rebase = revision(current, key, control)? != scope.header.expected();
    Ok(())
}

pub(in crate::mvcc) fn revision(
    view: &dyn CommittedRecordSnapshot,
    key: &[u8],
    control: &StorageReadControl,
) -> VersionResult<Option<crate::mvcc::CommitSequence>> {
    Ok(view
        .metadata(key, control)?
        .and_then(|record| record.revision))
}
pub(in crate::mvcc) fn validate(
    view: &dyn CommittedRecordSnapshot,
    mutation: usize,
    write: &PreparedRecordWrite,
    control: &StorageReadControl,
) -> VersionResult<()> {
    let actual = revision(view, write.key(), control)?;
    if actual != write.expected() {
        return Err(VersionError::WriteConflict {
            mutation,
            expected: write.expected(),
            actual,
        });
    }
    Ok(())
}
pub(in crate::mvcc) fn replace(
    changes: &PrivateRecordChanges,
    current: &dyn CommittedRecordSnapshot,
    key: &[u8],
    value: &[u8],
    control: &StorageReadControl,
) -> VersionResult<()> {
    changes.apply(
        &[RecordWrite {
            key,
            expected: revision(current, key, control)?,
            value: Some(value),
        }],
        control,
    )
}
pub(in crate::mvcc) fn bytes(
    record: Option<&crate::mvcc::RecordVersion<crate::mvcc::SharedRecordValue>>,
) -> Option<&[u8]> {
    record
        .and_then(|record| record.value())
        .map(|value| &***value)
}
