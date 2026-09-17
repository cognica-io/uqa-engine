//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Verify that the journal describes the canonical tensors actually being published.

use std::collections::BTreeMap;

use crate::{
    ivf_index::IVFMutation,
    mvcc::{
        commit::RecordWriteKind, CommittedRecordSnapshot, IVFRecordKey, IVFRecordLayout,
        PreparedRecordWrite, VersionError, VersionResult,
    },
    read_control::StorageReadControl,
};
use uqa_core::{memory::BudgetedVec, DocId};

pub(super) fn validate(
    metadata: &[u8],
    operations: &[IVFMutation<'_>],
    writes: &BTreeMap<&[u8], &PreparedRecordWrite>,
    base: &dyn CommittedRecordSnapshot,
    layout: &dyn IVFRecordLayout,
    control: &StorageReadControl,
) -> VersionResult<()> {
    let invalid = || VersionError::InvalidEncoding("IVF inputs disagree with canonical tensors");
    let mut ordered = BudgetedVec::new(control.memory());
    for (position, operation) in operations.iter().enumerate() {
        control.cancellation().check()?;
        let (document, vectors): (DocId, &[Vec<f32>]) = match operation {
            IVFMutation::Replace { document, vectors } => (*document, vectors),
            IVFMutation::Delete(document) => (*document, &[]),
            _ => return Err(invalid()),
        };
        ordered.push((document, position, vectors))?;
    }
    ordered.sort_unstable_by_key(|(document, position, _)| (*document, *position));
    let latest = |document| {
        let end = ordered.partition_point(|(id, _, _)| *id <= document);
        end.checked_sub(1)
            .and_then(|i| ordered.get(i))
            .filter(|(id, _, _)| *id == document)
            .map(|(_, _, vectors)| *vectors)
    };
    let prefix = layout.key(metadata, IVFRecordKey::Vectors, control)?;
    let mut count = 0;
    for (key, write) in writes.range::<[u8], _>((
        std::ops::Bound::Included(&*prefix),
        std::ops::Bound::Unbounded,
    )) {
        control.cancellation().check()?;
        if !key.starts_with(&prefix) {
            break;
        }
        if write.kind() != RecordWriteKind::Canonical {
            return Err(invalid());
        }
        let (document, ordinal) = layout.vector_id(key, control)?;
        let expected = latest(document).ok_or_else(invalid)?;
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
    }
    let mut required = 0_usize;
    for (position, (document, _, vectors)) in ordered.iter().enumerate() {
        control.cancellation().check()?;
        if ordered
            .get(position + 1)
            .is_none_or(|(next, _, _)| next != document)
        {
            required = required.checked_add(vectors.len()).ok_or_else(invalid)?;
        }
    }
    if count != required {
        return Err(invalid());
    }
    // Deleted tail rows must be in the sealed write set, even when their payloads were not needed by the mutation.
    base.visit_keys(&prefix, None, usize::MAX, control, &mut |key, record| {
        if record.live {
            let (document, ordinal) = layout.vector_id(key, control)?;
            if latest(document).is_some_and(|vectors| ordinal as usize >= vectors.len())
                && writes.get(key).is_none_or(|write| write.value().is_some())
            {
                return Err(invalid());
            }
        }
        Ok(true)
    })?;
    Ok(())
}
