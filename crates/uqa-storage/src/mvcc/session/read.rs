//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::mvcc::{MergedRecordSnapshot, VersionResult};
use crate::read_control::{KeyValueReadVisitor, StorageReadControl};

pub(super) fn visit_live(
    view: &MergedRecordSnapshot,
    prefix: &[u8],
    after: Option<&[u8]>,
    limit: usize,
    control: &StorageReadControl,
    visit: &mut KeyValueReadVisitor<'_>,
) -> VersionResult<()> {
    control.cancellation().check()?;
    if limit == 0 {
        return Ok(());
    }
    let mut count = 0;
    view.visit_prefix(prefix, after, usize::MAX, control, &mut |key, record| {
        if let Some(value) = record.value {
            visit(key, value)?;
            count += 1;
        }
        Ok(count < limit)
    })
}
