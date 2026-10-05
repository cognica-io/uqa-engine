//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Verify the final input of each document against canonical tensors without retaining the journal in memory.

use super::super::{layout::Layout, Key, VectorOperations};
use crate::{
    mvcc::{
        commit::{PreparedLookup, RecordWriteKind},
        CommittedRecordSnapshot, VersionError, VersionResult,
    },
    read_control::StorageReadControl,
    spill_map::Map,
};

pub(super) fn validate(
    metadata: &[u8],
    operations: &VectorOperations<'_>,
    writes: &PreparedLookup<'_>,
    base: &dyn CommittedRecordSnapshot,
    layout: Layout<'_>,
    control: &StorageReadControl,
) -> VersionResult<()> {
    let invalid = || VersionError::InvalidEncoding("vector inputs disagree with canonical tensors");
    let mut latest = Map::<u64>::new(control.memory(), control.memory().limit() / 64);
    operations.visit(control, |position, operation| {
        latest.insert(u128::from(operation.document), position, Some(control))?;
        Ok(())
    })?;
    let prefix = layout.key(metadata, Key::Vectors, control)?;
    let mut count = 0;
    writes.visit_prefix(&prefix, control, &mut |write| {
        let key = write.key();
        if write.kind() != RecordWriteKind::Canonical {
            return Err(invalid());
        }
        let (document, ordinal) = layout.vector_id(key, control)?;
        let position = latest.get(u128::from(document))?.ok_or_else(invalid)?;
        let input = operations.get(*position)?;
        let expected = input.vectors();
        match write.value() {
            Some(value) => {
                let expected = expected.get(ordinal as usize).ok_or_else(invalid)?;
                let (_, _, actual) = layout.vector(key, value, control)?;
                if actual.len() != expected.len()
                    || actual
                        .iter()
                        .zip(expected)
                        .any(|(a, b)| a.to_bits() != b.to_bits())
                {
                    return Err(invalid());
                }
                count += 1;
            }
            None if (ordinal as usize) < expected.len() => return Err(invalid()),
            None => {}
        }
        Ok(true)
    })?;
    let mut required = 0_usize;
    for entry in latest.iter() {
        control.check()?;
        let (_, position) = entry?;
        let input = operations.get(*position)?;
        required = required
            .checked_add(input.vectors().len())
            .ok_or_else(invalid)?;
    }
    if count != required {
        return Err(invalid());
    }
    // Deleted tail rows must remain in the sealed write set, even when the mutation did not need their payloads.
    base.visit_keys(&prefix, None, usize::MAX, control, &mut |key, record| {
        if record.live {
            let (document, ordinal) = layout.vector_id(key, control)?;
            if let Some(position) = latest.get(u128::from(document))? {
                let input = operations.get(*position)?;
                if ordinal as usize >= input.vectors().len()
                    && writes
                        .get(key, control)?
                        .is_none_or(|write| write.value().is_some())
                {
                    return Err(invalid());
                }
            }
        }
        Ok(true)
    })?;
    Ok(())
}
