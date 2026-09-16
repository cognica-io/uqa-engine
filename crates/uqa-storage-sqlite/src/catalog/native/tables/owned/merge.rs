//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Probe row identities before merging standalone names, without hydrating values or reentering a visitor.

use super::{Family, NativeRecordOwner, NativeSnapshot, Result, SQLiteError};
use crate::mvcc::native::NativeRecordIdentity;
use uqa_core::memory::BudgetedVec;
use uqa_storage::mvcc::VersionError;

impl NativeSnapshot {
    pub(in crate::catalog::native) fn has_table_data(
        &self,
        owner: NativeRecordOwner,
    ) -> Result<bool> {
        for family in Family::all().filter(|family| family.layout().columns.contains(&"table_name"))
        {
            let prefix =
                NativeRecordIdentity::new(family, owner)?.encode_prefix(&[], &self.control)?;
            let mut found = false;
            self.view.visit_keys(
                &prefix,
                None,
                usize::MAX,
                &self.control,
                &mut |_, record| {
                    found = record.live;
                    Ok(!found)
                },
            )?;
            if found {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(in crate::catalog::native) fn check_table_data_merge(
        &self,
        from: NativeRecordOwner,
        to: NativeRecordOwner,
    ) -> Result<()> {
        for family in Family::all().filter(|family| family.layout().columns.contains(&"table_name"))
        {
            let source =
                NativeRecordIdentity::new(family, from)?.encode_prefix(&[], &self.control)?;
            let target =
                NativeRecordIdentity::new(family, to)?.encode_prefix(&[], &self.control)?;
            let mut after = BudgetedVec::new(self.control.memory());
            loop {
                let mut next = BudgetedVec::new(self.control.memory());
                self.view.visit_keys(
                    &source,
                    (!after.is_empty()).then_some(&after[..]),
                    usize::MAX,
                    &self.control,
                    &mut |key, record| {
                        if record.live {
                            next.extend_from_slice(key).map_err(VersionError::from)?;
                        }
                        Ok(!record.live)
                    },
                )?;
                if next.is_empty() {
                    break;
                }
                let mut key = BudgetedVec::new(self.control.memory());
                key.extend_from_slice(&target).map_err(VersionError::from)?;
                key.extend_from_slice(&next[source.len()..])
                    .map_err(VersionError::from)?;
                if self
                    .view
                    .metadata(&key, &self.control)?
                    .is_some_and(|record| record.live)
                {
                    return Err(SQLiteError::StorageBackend(format!(
                        "renamed table data conflicts in {}",
                        family.layout().table
                    )));
                }
                after = next;
            }
        }
        Ok(())
    }
}
