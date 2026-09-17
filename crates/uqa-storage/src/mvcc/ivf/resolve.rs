//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Merge only evaluated IVF inputs; canonical replacements keep their original preconditions.

mod canonical;
mod load;

use super::{IVFRecordHeader, IVFRecordKey as Key, IVFRecordLayout, IVFRecordValue as Value};
use crate::mvcc::{
    commit::RecordWriteKind, CommittedRecordSnapshot, PreparedRecordCommit, PreparedRecordWrite,
    PrivateRecordChanges, RecordWrite, VersionError, VersionResult,
};
use crate::{ivf_index::IVFMutation, read_control::StorageReadControl};
use std::collections::BTreeMap;
use uqa_core::memory::{BudgetedVec, MemoryError};

struct Scope<'a> {
    header: &'a PreparedRecordWrite,
    operations: BudgetedVec<IVFMutation<'a>>,
    rebase: bool,
}

pub(in crate::mvcc) fn resolve(
    original: &PreparedRecordCommit,
    base: &dyn CommittedRecordSnapshot,
    current: &dyn CommittedRecordSnapshot,
    layout: &dyn IVFRecordLayout,
    control: &StorageReadControl,
) -> VersionResult<PreparedRecordCommit> {
    let effects = original.ivf.as_ref().expect("requested IVF effects");
    let lookup_bytes = original
        .records()
        .len()
        .checked_mul(size_of::<(&[u8], &PreparedRecordWrite)>())
        .and_then(|n| {
            effects
                .operations
                .len()
                .checked_mul(size_of::<(&[u8], Scope<'_>)>())
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
    let mut scopes = BTreeMap::<&[u8], Scope<'_>>::new();
    for operation in effects.operations.iter() {
        control.cancellation().check()?;
        let key = operation.metadata.bytes();
        let scope = if let std::collections::btree_map::Entry::Vacant(entry) = scopes.entry(key) {
            let header = *writes.get(key).ok_or(VersionError::InvalidEncoding(
                "IVF input lacks a metadata replacement",
            ))?;
            entry.insert(Scope {
                header,
                operations: BudgetedVec::new(control.memory()),
                rebase: false,
            })
        } else {
            scopes.get_mut(key).expect("existing scope")
        };
        scope.operations.push(operation.borrowed())?;
    }
    for (key, scope) in &mut scopes {
        validate_scope(key, scope, &writes, base, current, layout, control)?;
    }
    let changes = PrivateRecordChanges::new(control.memory());
    for (position, write) in original.records().iter().enumerate() {
        control.cancellation().check()?;
        if write.kind() != RecordWriteKind::IVFPreview {
            changes.apply_owned(std::slice::from_ref(write), control)?;
            continue;
        }
        let owner =
            layout
                .metadata_key(write.key(), control)?
                .ok_or(VersionError::InvalidEncoding(
                    "IVF preview targets a canonical record",
                ))?;
        let scope = scopes.get(&*owner).ok_or(VersionError::InvalidEncoding(
            "IVF preview lacks evaluated document input",
        ))?;
        if !scope.rebase {
            validate(current, position, write, control)?;
            changes.apply_owned(
                &[write.clone().with_kind(RecordWriteKind::Canonical)],
                control,
            )?;
        }
    }
    for (key, scope) in scopes.iter().filter(|(_, scope)| scope.rebase) {
        merge_scope(key, scope, &changes, current, layout, control)?;
    }
    Ok(changes
        .prepare(control)?
        .retain_graph_effects(original, control)?
        .resolved(original, current.sequence()))
}

fn validate_scope(
    key: &[u8],
    scope: &mut Scope<'_>,
    writes: &BTreeMap<&[u8], &PreparedRecordWrite>,
    base: &dyn CommittedRecordSnapshot,
    current: &dyn CommittedRecordSnapshot,
    layout: &dyn IVFRecordLayout,
    control: &StorageReadControl,
) -> VersionResult<()> {
    if scope.header.kind() != RecordWriteKind::IVFPreview {
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
                "IVF input mixes with unjournaled derived records",
            ));
        }
    }
    let base_row = base.get(key, control)?;
    let latest_row = current.get(key, control)?;
    let old = bytes(base_row.as_ref()).ok_or(VersionError::InvalidEncoding(
        "IVF input has no committed definition",
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
            "IVF preview removed its definition",
        ))?,
        control,
    )?;
    if !same_definition(before, now) || !same_definition(before, evaluated) {
        return Err(VersionError::WriteConflict {
            mutation: 0,
            expected: scope.header.expected(),
            actual: revision(current, key, control)?,
        });
    }
    if before.revision.checked_add(scope.operations.len() as u64) != Some(evaluated.revision) {
        return Err(VersionError::InvalidEncoding(
            "IVF preview does not match its ordered input journal",
        ));
    }
    canonical::validate(key, &scope.operations, writes, base, layout, control)?;
    scope.rebase = revision(current, key, control)? != scope.header.expected();
    Ok(())
}

fn merge_scope(
    key: &[u8],
    scope: &Scope<'_>,
    changes: &PrivateRecordChanges,
    current: &dyn CommittedRecordSnapshot,
    layout: &dyn IVFRecordLayout,
    control: &StorageReadControl,
) -> VersionResult<()> {
    control.cancellation().check()?;
    let row = current.get(key, control)?;
    let template = bytes(row.as_ref()).ok_or(VersionError::InvalidEncoding(
        "missing current IVF definition",
    ))?;
    let header = layout.header(key, template, control)?;
    let index = load::index(key, header, current, layout, control)?;
    let snapshot = index.prepare_metadata_changes(&scope.operations, control)?;
    let next = header
        .revision
        .checked_add(scope.operations.len() as u64)
        .ok_or(VersionError::InvalidEncoding("IVF revision exhausted"))?;
    for address in [Key::Centroids, Key::Assignments] {
        let prefix = layout.key(key, address, control)?;
        current.visit_keys(&prefix, None, usize::MAX, control, &mut |key, record| {
            if record.live {
                changes.apply(
                    &[RecordWrite {
                        key,
                        expected: record.revision,
                        value: None,
                    }],
                    control,
                )?;
            }
            Ok(true)
        })?;
    }
    let header_value = layout.encode(
        key,
        template,
        Value::Header {
            snapshot: &snapshot,
            revision: next,
        },
        control,
    )?;
    replace(changes, current, key, &header_value, control)?;
    for (centroid, vector) in snapshot.centroids.iter().enumerate() {
        let address = layout.key(key, Key::Centroid(centroid), control)?;
        let value = layout.encode(&address, template, Value::Centroid(vector), control)?;
        replace(changes, current, &address, &value, control)?;
    }
    for (document, ordinal, centroid) in &snapshot.assignments {
        let address = layout.key(key, Key::Assignment(*document, *ordinal), control)?;
        let value = layout.encode(&address, template, Value::Assignment(*centroid), control)?;
        replace(changes, current, &address, &value, control)?;
    }
    Ok(())
}

fn same_definition(a: IVFRecordHeader, b: IVFRecordHeader) -> bool {
    a.dimensions == b.dimensions && a.params == b.params
}
fn revision(
    view: &dyn CommittedRecordSnapshot,
    key: &[u8],
    control: &StorageReadControl,
) -> VersionResult<Option<crate::mvcc::CommitSequence>> {
    Ok(view
        .metadata(key, control)?
        .and_then(|record| record.revision))
}
fn validate(
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
fn replace(
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
fn bytes(
    record: Option<&crate::mvcc::RecordVersion<crate::mvcc::SharedRecordValue>>,
) -> Option<&[u8]> {
    record
        .and_then(|record| record.value())
        .map(|value| &***value)
}
