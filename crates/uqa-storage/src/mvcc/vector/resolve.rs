//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Merge only evaluated vector inputs; canonical replacements keep their original preconditions.

mod canonical;

use super::{layout::Layout, IndexKind, Key, VectorOperations};
use crate::mvcc::{
    commit::{PreparedLookup, RecordWriteKind},
    key::RecordKey,
    resolution::ResolutionMode,
    CommittedRecordSnapshot, PreparedRecordCommit, PreparedRecordWrite, PrivateRecordChanges,
    RecordWrite, VersionError, VersionResult,
};
use crate::read_control::StorageReadControl;
use uqa_core::memory::BudgetedMap;

struct Scope<'a> {
    layout: Layout<'a>,
    kind: IndexKind,
    header: PreparedRecordWrite,
    operations: VectorOperations<'a>,
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
    let writes = PreparedLookup::new(original, control)?;
    let mut originals = original.writes();
    let mut position = 0;
    while let Some(write) = originals.next_metadata(control)? {
        if write.kind() == RecordWriteKind::Canonical {
            validate(current, position, write.key(), write.expected(), control)?;
        }
        position += 1;
    }
    let mut scopes = BudgetedMap::<(IndexKind, RecordKey), Scope<'_>>::new(control.memory());
    for entry in effects.operations.iter() {
        control.check()?;
        let (position, operation) = entry?;
        let key = (
            operation.kind,
            RecordKey::new(&operation.metadata, control.memory())?,
        );
        if !scopes.contains_key(&key) {
            let header =
                writes
                    .get(&operation.metadata, control)?
                    .ok_or(VersionError::InvalidEncoding(
                        "vector input lacks a metadata replacement",
                    ))?;
            scopes.insert(
                key.clone(),
                Scope {
                    layout: operation.kind.layout(persistence)?,
                    kind: operation.kind,
                    header,
                    operations: VectorOperations::new(&effects.operations, control),
                    rebase: false,
                },
            )?;
        }
        scopes
            .get_mut(&key)
            .expect("existing scope")
            .operations
            .push(position, control)?;
    }
    let mut validation = Ok(());
    scopes.for_each_mut(|(_, key), scope| {
        if validation.is_ok() {
            validation = validate_scope(
                key.bytes(),
                scope,
                original,
                &writes,
                base,
                current,
                control,
            );
        }
    });
    validation?;
    let changes = PrivateRecordChanges::new(control.memory());
    let mut originals = original.writes();
    let mut position = 0;
    while let Some(write) = originals.next(control)? {
        let write = &write;
        position += 1;
        let position = position - 1;
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
        let scope = scopes.get(&(kind, RecordKey::from_budgeted(owner))).ok_or(
            VersionError::InvalidEncoding("vector preview lacks evaluated document input"),
        )?;
        if !scope.rebase {
            validate(current, position, write.key(), write.expected(), control)?;
            changes.apply_owned(&[write.clone().with_kind(mode.kind(write.kind()))], control)?;
        }
    }
    for ((_, key), scope) in scopes.iter().filter(|(_, scope)| scope.rebase) {
        scope.merge(key.bytes(), &changes, current, mode, control)?;
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
        let merged = merged.prepare(control)?;
        let mut merged_writes = merged.writes();
        while let Some(write) = merged_writes.next(control)? {
            changes.apply_owned(&[write.with_kind(mode.kind(self.kind.preview()))], control)?;
        }
        Ok(())
    }
}

fn validate_scope(
    key: &[u8],
    scope: &mut Scope<'_>,
    original: &PreparedRecordCommit,
    writes: &PreparedLookup<'_>,
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
    if writes.contains(&guard, control)? {
        return Ok(());
    }
    let mut originals = original.writes();
    while let Some(write) = originals.next_metadata(control)? {
        if write.kind() == RecordWriteKind::Canonical
            && layout.metadata_key(write.key(), control)?.as_deref() == Some(key)
        {
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
    key: &[u8],
    expected: Option<crate::mvcc::CommitSequence>,
    control: &StorageReadControl,
) -> VersionResult<()> {
    let actual = revision(view, key, control)?;
    if actual != expected {
        return Err(VersionError::WriteConflict {
            mutation,
            expected,
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
