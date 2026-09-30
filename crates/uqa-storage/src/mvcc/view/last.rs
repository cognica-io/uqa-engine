//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Greatest-key selection preserves private precedence and tombstone visibility.

use uqa_core::memory::BudgetedVec;

use super::{CommittedRecordSnapshot, MergedRecordSnapshot, RecordKeyVisitor, RecordMetadata};
use crate::{mvcc::VersionResult, read_control::StorageReadControl};

pub(super) fn by_scan<T: CommittedRecordSnapshot + ?Sized>(
    snapshot: &T,
    prefix: &[u8],
    before: Option<&[u8]>,
    control: &StorageReadControl,
    visit: &mut RecordKeyVisitor<'_>,
) -> VersionResult<()> {
    control.check()?;
    let mut key = BudgetedVec::new(control.memory());
    let mut found = None;
    snapshot.visit_keys(prefix, None, usize::MAX, control, &mut |next, record| {
        control.check()?;
        if before.is_some_and(|before| next >= before) {
            return Ok(false);
        }
        key.clear();
        key.extend_from_slice(next)?;
        found = Some(record);
        Ok(true)
    })?;
    if let Some(record) = found {
        visit(&key, record)?;
    }
    control.check()?;
    Ok(())
}

impl MergedRecordSnapshot {
    /// Select the greatest visible committed/private identity strictly below a bound, including tombstones. A private replacement wins for the same key; neither selection materializes record values.
    pub fn visit_last_key(
        &self,
        prefix: &[u8],
        before: Option<&[u8]>,
        control: &StorageReadControl,
        visit: &mut RecordKeyVisitor<'_>,
    ) -> VersionResult<()> {
        control.check()?;
        let private = self.private.last_before(prefix, before, control)?;
        let mut visited = false;
        self.committed
            .visit_last_key(prefix, before, control, &mut |key, record| {
                visited = true;
                if let Some(write) = private.as_ref().filter(|write| write.key() >= key) {
                    visit(
                        write.key(),
                        RecordMetadata {
                            revision: write.expected(),
                            live: write.value().is_some(),
                        },
                    )?;
                } else {
                    visit(key, record)?;
                }
                Ok(false)
            })?;
        if !visited {
            if let Some(write) = private {
                visit(
                    write.key(),
                    RecordMetadata {
                        revision: write.expected(),
                        live: write.value().is_some(),
                    },
                )?;
            }
        }
        control.check()?;
        Ok(())
    }
}
