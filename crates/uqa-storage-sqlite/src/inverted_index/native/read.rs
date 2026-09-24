//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded projected reads retain native owner and committed/private visibility.

use super::records::{self, invalid};
use crate::mvcc::native::{decode_record, NativeRecordOwner, NativeSnapshot};
use std::sync::Arc;
use uqa_core::memory::{BudgetedVec, MemoryReservation};
use uqa_storage::key_value::{
    occurrence_format::{OccurrenceAddress as Address, OccurrenceProjection as Projection},
    KeyValueRead, KeyValueReadRevision,
};
use uqa_storage::read_control::{KeyValueReadVisitor, StorageReadControl, ValueReadVisitor};
use uqa_storage::{mvcc::VersionError, StorageBackendResult};

pub(super) struct NativeRead {
    pub(super) snapshot: NativeSnapshot,
    pub(super) table: String,
    pub(super) owner: Option<NativeRecordOwner>,
    identity: KeyValueReadRevision,
    _memory: MemoryReservation,
}

impl NativeRead {
    pub(super) fn new(snapshot: &NativeSnapshot, table: &str) -> StorageBackendResult<Self> {
        snapshot.control.check()?;
        let memory = snapshot.control.memory().reserve(
            std::mem::size_of::<Self>()
                .checked_add(table.len())
                .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?,
        )?;
        let owner = snapshot.table_owner(table)?;
        Ok(Self {
            snapshot: NativeSnapshot {
                view: snapshot
                    .view
                    .try_clone()
                    .map_err(VersionError::into_storage_error)?,
                control: snapshot.control.clone(),
                database: snapshot.database,
            },
            table: table.to_owned(),
            owner,
            identity: KeyValueReadRevision::fresh(),
            _memory: memory,
        })
    }

    pub(super) fn address<'a>(&self, key: &'a [u8]) -> StorageBackendResult<Address<'a>> {
        let address = Address::decode(key)?;
        if address.table != self.table {
            return Err(invalid("occurrence projection belongs to another table"));
        }
        Ok(address)
    }

    pub(super) fn row<T>(
        &self,
        address: Address<'_>,
        control: &StorageReadControl,
        read: impl FnOnce(Option<&[rusqlite::types::ValueRef<'_>]>) -> StorageBackendResult<T>,
    ) -> StorageBackendResult<T> {
        if !address.complete() {
            return Err(invalid("occurrence value requires a complete address"));
        }
        records::components(address, control)?;
        let Some(owner) = self.owner else {
            return read(None);
        };
        let key = records::key(address, owner, control)?;
        let mut read = Some(read);
        let mut result = None;
        self.snapshot
            .view
            .visit_value(&key, control, &mut |record| {
                let value = record.and_then(|record| record.value);
                let row = value
                    .map(|bytes| decode_record(&key, bytes, control))
                    .transpose()?;
                result = Some(read.take().expect("one native value visitor")(
                    row.as_ref().map(|(_, row)| &**row),
                )?);
                Ok(())
            })
            .map_err(VersionError::into_storage_error)?;
        result.ok_or_else(|| invalid("native occurrence read did not visit its value"))
    }

    pub(super) fn keys(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
        control: &StorageReadControl,
    ) -> StorageBackendResult<BudgetedVec<BudgetedVec<u8>>> {
        let address = self.address(prefix)?;
        let mut keys = BudgetedVec::new(control.memory());
        control.check()?;
        let Some(owner) = self.owner else {
            return Ok(keys);
        };
        if limit == 0 {
            return Ok(keys);
        }
        let selected = address
            .projection
            .as_ref()
            .map_or(records::CURRENT.as_slice(), std::slice::from_ref);
        for &projection in selected {
            if projection.is_legacy() {
                return Err(invalid("legacy occurrence values require a source rebuild"));
            }
            let address = Address {
                projection: Some(projection),
                ..address
            };
            let native_prefix = records::prefix(address, owner, control)?;
            // A fixed field/term prefix has the same cluster order in both formats. Cursor paging therefore reads only the requested page's keys.
            let ordered = matches!(
                projection,
                Projection::Score | Projection::Positions | Projection::Skip | Projection::BlockMax
            ) && address.field.is_some()
                && address.term.is_some();
            let native_after = if ordered {
                after
                    .filter(|after| after.starts_with(prefix))
                    .map(|after| {
                        let after = self.address(after)?;
                        records::key(after, owner, control)
                    })
                    .transpose()?
            } else {
                None
            };
            self.snapshot
                .view
                .visit_keys(
                    &native_prefix,
                    native_after.as_deref(),
                    usize::MAX,
                    control,
                    &mut |native_key, metadata| {
                        if metadata.live {
                            let key = records::address_from_key(
                                native_key,
                                &self.table,
                                projection,
                                control,
                            )?;
                            if key.starts_with(prefix) && after.is_none_or(|after| &*key > after) {
                                keys.push(key)?;
                            }
                        }
                        Ok(!ordered || keys.len() < limit)
                    },
                )
                .map_err(VersionError::into_storage_error)?;
        }
        keys.sort_unstable_by(|a, b| a.as_ref().cmp(b.as_ref()));
        keys.truncate(limit);
        Ok(keys)
    }
}

impl KeyValueRead for NativeRead {
    fn control(&self) -> &StorageReadControl {
        &self.snapshot.control
    }
    fn revision(&self, _: &[&[u8]]) -> StorageBackendResult<KeyValueReadRevision> {
        self.control().check()?;
        Ok(self.identity.clone())
    }
    fn retain(&self, _: &[&[u8]]) -> StorageBackendResult<Arc<dyn KeyValueRead + Send + Sync>> {
        let mut retained = Self::new(&self.snapshot, &self.table)?;
        retained.identity = self.identity.clone();
        Ok(Arc::new(retained))
    }
    fn visit_value(
        &self,
        key: &[u8],
        visit: &mut ValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.visit_value_budgeted(key, self.control(), visit)
    }
    fn visit_prefix(
        &self,
        prefix: &[u8],
        visit: &mut KeyValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.visit_prefix_after(prefix, None, usize::MAX, self.control(), visit)
    }
    fn visit_value_budgeted(
        &self,
        key: &[u8],
        control: &StorageReadControl,
        visit: &mut ValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        control.check()?;
        let address = self.address(key)?;
        self.row(address, control, |row| match row {
            Some(row) => records::project(
                row,
                address.projection.expect("complete address"),
                control,
                |bytes| visit(Some(bytes)),
            ),
            None => visit(None),
        })?;
        control.check()
    }
    fn visit_prefix_after(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
        control: &StorageReadControl,
        visit: &mut KeyValueReadVisitor<'_>,
    ) -> StorageBackendResult<()> {
        for key in self.keys(prefix, after, limit, control)?.iter() {
            self.visit_value_budgeted(key, control, &mut |value| {
                visit(
                    key,
                    value.ok_or_else(|| {
                        invalid("native occurrence key lost its fixed-view value")
                    })?,
                )
            })?;
        }
        control.check()
    }
    fn contains_prefix_budgeted(
        &self,
        prefix: &[u8],
        control: &StorageReadControl,
    ) -> StorageBackendResult<bool> {
        control.check()?;
        let address = self.address(prefix)?;
        let Some(owner) = self.owner else {
            return Ok(false);
        };
        let selected = address
            .projection
            .as_ref()
            .map_or(records::CURRENT.as_slice(), std::slice::from_ref);
        for &projection in selected {
            if records::family(projection).is_none() {
                continue;
            }
            let native_prefix = records::prefix(
                Address {
                    projection: Some(projection),
                    ..address
                },
                owner,
                control,
            )?;
            let mut found = false;
            self.snapshot
                .view
                .visit_keys(
                    &native_prefix,
                    None,
                    usize::MAX,
                    control,
                    &mut |key, record| {
                        if record.live {
                            found = projection.is_legacy()
                                || records::address_from_key(
                                    key,
                                    &self.table,
                                    projection,
                                    control,
                                )?
                                .starts_with(prefix);
                        }
                        Ok(!found)
                    },
                )
                .map_err(VersionError::into_storage_error)?;
            if found {
                return Ok(true);
            }
        }
        Ok(false)
    }
}
