//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepare a matching IVF generation from retained document inputs.

mod load;
use super::{IVFRecordKey as Key, IVFRecordLayout, IVFRecordValue as Value};
use crate::mvcc::vector::resolve::{bytes, replace};
use crate::mvcc::{
    CommittedRecordSnapshot, PrivateRecordChanges, RecordWrite, VersionError, VersionResult,
};
use crate::read_control::StorageReadControl;
use uqa_core::memory::BudgetedVec;

pub(in crate::mvcc) fn merge(
    key: &[u8],
    operations: &[crate::mvcc::vector::Mutation<'_>],
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
    let mut inputs = BudgetedVec::new(control.memory());
    for operation in operations {
        inputs.push(operation.ivf())?;
    }
    let snapshot = index.prepare_metadata_changes(&inputs, control)?;
    let next = header
        .revision
        .map(|revision| {
            revision
                .checked_add(operations.len() as u64)
                .ok_or(VersionError::InvalidEncoding("IVF revision exhausted"))
        })
        .transpose()?;
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
