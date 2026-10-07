//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Document-major projections reorder only one document's field names.

use super::{records, Address, BudgetedVec, NativeRead, Projection, StorageBackendResult};
use uqa_storage::{
    mvcc::VersionError,
    read_control::{KeyReadVisitor, StorageReadControl},
};

impl NativeRead {
    pub(super) fn visit_document_keys(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
        control: &StorageReadControl,
        visit: &mut KeyReadVisitor<'_>,
    ) -> StorageBackendResult<bool> {
        let address = self.address(prefix)?;
        let Some(projection @ (Projection::Length | Projection::Document)) = address.projection
        else {
            return Ok(false);
        };
        self.snapshot.control.check()?;
        control.check()?;
        let Some(owner) = self.owner else {
            return Ok(true);
        };
        if limit == 0 {
            return Ok(true);
        }
        let native_prefix = records::prefix(address, owner, control)?;
        // A complete continuation can skip earlier documents, but the field
        // suffix must still be compared in the common length-prefixed order.
        let native_after = after
            .filter(|key| key.starts_with(prefix))
            .and_then(|key| Address::decode(key).ok())
            .filter(|key| key.complete() && address.document.is_none())
            .map(|key| records::prefix(Address { field: None, ..key }, owner, control))
            .transpose()?;
        let mut document = None;
        let mut keys = BudgetedVec::new(control.memory());
        let mut remaining = limit;
        let mut emit = |keys: &mut BudgetedVec<BudgetedVec<u8>>| {
            keys.sort_unstable_by(|a, b| a.as_ref().cmp(b.as_ref()));
            for key in keys.iter() {
                self.snapshot.control.check()?;
                control.check()?;
                if key.starts_with(prefix) && after.is_none_or(|after| key.as_ref() > after) {
                    visit(key)?;
                    remaining -= 1;
                    if remaining == 0 {
                        break;
                    }
                }
            }
            keys.clear();
            Ok::<_, uqa_storage::StorageBackendError>(remaining != 0)
        };
        let mut more = true;
        self.snapshot
            .view
            .visit_keys(
                &native_prefix,
                native_after.as_deref(),
                usize::MAX,
                control,
                &mut |key, metadata| {
                    if !metadata.live {
                        return Ok(true);
                    }
                    let key = records::address_from_key(key, &self.table, projection, control)?;
                    let next = Address::decode(&key)?.document;
                    if document != next && !keys.is_empty() {
                        more = emit(&mut keys)?;
                        if !more {
                            return Ok(false);
                        }
                    }
                    document = next;
                    keys.push(key)?;
                    Ok(true)
                },
            )
            .map_err(VersionError::into_storage_error)?;
        if more {
            emit(&mut keys)?;
        }
        self.snapshot.control.check()?;
        control.check()?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests;
