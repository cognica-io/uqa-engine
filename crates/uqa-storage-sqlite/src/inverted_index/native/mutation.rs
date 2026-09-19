//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Projected changes coalesce into complete native rows before the provider stages any result.

mod encoding;

use super::{
    read::NativeRead,
    records::{self, invalid},
};
use crate::mvcc::native::NativeRecordOwner;
use std::collections::BTreeMap;
use uqa_core::memory::{BudgetedVec, MemoryReservation};
use uqa_storage::key_value::occurrence_format::{
    OccurrenceAddress as Address, OccurrenceProjection as Projection,
};
use uqa_storage::{KeyValueBatch, StorageBackendResult};

struct Draft {
    address: BudgetedVec<u8>,
    values: [Option<BudgetedVec<u8>>; 2],
    _key: MemoryReservation,
    _entry: MemoryReservation,
    merge: bool,
}

pub(super) struct ProjectionBatch<'a> {
    read: &'a NativeRead,
    native: &'a mut dyn KeyValueBatch,
    owner: Option<NativeRecordOwner>,
    rows: BTreeMap<Vec<u8>, Draft>,
    cleared: bool,
}

impl<'a> ProjectionBatch<'a> {
    pub(super) fn new(read: &'a NativeRead, native: &'a mut dyn KeyValueBatch) -> Self {
        Self {
            read,
            native,
            owner: read.owner,
            rows: BTreeMap::new(),
            cleared: false,
        }
    }

    fn ensure_owner(&mut self) -> StorageBackendResult<NativeRecordOwner> {
        if let Some(owner) = self.owner {
            return Ok(owner);
        }
        let owner = self
            .read
            .snapshot
            .ensure_table_owner(&self.read.table, self.native)?;
        self.owner = Some(owner);
        Ok(owner)
    }

    fn change(
        &mut self,
        key: &[u8],
        value: Option<&[u8]>,
        merge: bool,
    ) -> StorageBackendResult<()> {
        let owner = self.ensure_owner()?;
        let control = &self.read.snapshot.control;
        control.check()?;
        let address = self.read.address(key)?;
        if !address.complete() {
            return Err(invalid("occurrence mutation requires a complete address"));
        }
        let projection = address.projection.expect("complete address");
        let projections = records::projections(
            records::family(projection).expect("complete address has a family"),
        );
        let slot = projections
            .iter()
            .position(|&candidate| candidate == projection)
            .expect("current occurrence projection");
        let (key, key_memory) = records::key(address, owner, control)?.into_parts();
        let entry = self.rows.entry(key);
        let draft = match entry {
            std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::btree_map::Entry::Vacant(entry) => {
                let memory = control
                    .memory()
                    .reserve(std::mem::size_of::<(Vec<u8>, Draft)>())?;
                let mut values = [None, None];
                if !self.cleared && projections.len() > 1 {
                    self.read.row(address, control, |row| {
                        if let Some(row) = row {
                            for (slot, &projection) in projections.iter().enumerate() {
                                values[slot] =
                                    Some(records::project(row, projection, control, |bytes| {
                                        let mut value = BudgetedVec::new(control.memory());
                                        value.extend_from_slice(bytes)?;
                                        Ok(value)
                                    })?);
                            }
                        }
                        Ok(())
                    })?;
                }
                entry.insert(Draft {
                    address: address.encode(control)?,
                    values,
                    _key: key_memory,
                    _entry: memory,
                    merge,
                })
            }
        };
        draft.merge &= merge;
        draft.values[slot] = value
            .map(|bytes| {
                let mut value = BudgetedVec::new(control.memory());
                value.extend_from_slice(bytes)?;
                Ok::<_, uqa_storage::StorageBackendError>(value)
            })
            .transpose()?;
        Ok(())
    }

    pub(super) fn flush(self) -> StorageBackendResult<()> {
        let Some(owner) = self.owner else {
            return Ok(());
        };
        for (key, draft) in self.rows {
            self.read.snapshot.control.check()?;
            if draft.values.iter().all(Option::is_none) {
                if draft.merge {
                    self.native.replace_occurrence_record(&key, None)?;
                } else {
                    self.native.delete(&key)?;
                }
            } else {
                encoding::write(self.native, self.read, owner, &draft)?;
            }
        }
        Ok(())
    }
}

impl KeyValueBatch for ProjectionBatch<'_> {
    fn observe_identifier(&mut self, namespace: &[u8], value: u64) -> StorageBackendResult<()> {
        self.native.observe_identifier(namespace, value)
    }
    fn inherit_identifiers(&mut self, from: &[u8], to: &[u8]) -> StorageBackendResult<()> {
        self.native.inherit_identifiers(from, to)
    }
    fn put(&mut self, key: &[u8], value: &[u8]) -> StorageBackendResult<()> {
        self.change(key, Some(value), false)
    }
    fn delete(&mut self, key: &[u8]) -> StorageBackendResult<()> {
        self.change(key, None, false)
    }
    fn replace_occurrence_record(
        &mut self,
        key: &[u8],
        value: Option<&[u8]>,
    ) -> StorageBackendResult<()> {
        self.change(key, value, true)
    }
    fn invalidate_occurrence_prefix(&mut self, prefix: &[u8]) -> StorageBackendResult<()> {
        let address = self.read.address(prefix)?;
        if !matches!(
            address.projection,
            Some(Projection::Skip | Projection::BlockMax)
        ) {
            return Err(invalid("invalid native occurrence cache prefix"));
        }
        if let Some(owner) = self.owner {
            self.native.invalidate_occurrence_prefix(&records::prefix(
                address,
                owner,
                &self.read.snapshot.control,
            )?)?;
        }
        Ok(())
    }
    fn occurrence_document(&mut self, table: &str, document: u64) -> StorageBackendResult<()> {
        if table != self.read.table {
            return Err(invalid("native occurrence document changed table"));
        }
        let owner = self.ensure_owner()?;
        let document = crate::inverted_index::encode_index_u64("document", document)?;
        self.read
            .snapshot
            .occurrence_guard(self.native, table, owner, document)?;
        Ok(())
    }
    fn reset_occurrences(&mut self, table: &str) -> StorageBackendResult<()> {
        if table != self.read.table {
            return Err(invalid("native occurrence reset changed table"));
        }
        let owner = self.ensure_owner()?;
        self.read
            .snapshot
            .reset_occurrence_rows(self.native, owner)?;
        Ok(())
    }
    fn delete_prefix(&mut self, prefix: &[u8]) -> StorageBackendResult<()> {
        let address = self.read.address(prefix)?;
        let control = &self.read.snapshot.control;
        control.check()?;
        match address.projection {
            None => {
                // Whole-index replacement retires rows without decoding discarded payloads. Later puts must not inherit either column from an original row.
                if let Some(owner) = self.owner {
                    for projection in [
                        Projection::Score,
                        Projection::Skip,
                        Projection::BlockMax,
                        Projection::Document,
                        Projection::Length,
                        Projection::Field,
                        Projection::Format,
                    ] {
                        self.native.delete_prefix(&records::prefix(
                            Address {
                                projection: Some(projection),
                                ..address
                            },
                            owner,
                            control,
                        )?)?;
                    }
                }
                self.rows.clear();
                self.cleared = true;
                return Ok(());
            }
            Some(projection) if projection.is_legacy() => {
                if let Some(owner) = self.owner.filter(|_| records::family(projection).is_some()) {
                    self.native
                        .delete_prefix(&records::prefix(address, owner, control)?)?;
                }
                return Ok(());
            }
            _ => {}
        }
        if !self.cleared {
            for key in self.read.keys(prefix, None, usize::MAX, control)?.iter() {
                self.delete(key)?;
            }
        }
        // Include rows introduced earlier in this same callback, preserving delete-after-put order.
        for draft in self.rows.values_mut() {
            control.check()?;
            let address = Address::decode(&draft.address)?;
            let family = records::family(address.projection.expect("complete address"))
                .expect("current family");
            for (slot, &projection) in records::projections(family).iter().enumerate() {
                if (Address {
                    projection: Some(projection),
                    ..address
                })
                .encode(control)?
                .starts_with(prefix)
                {
                    draft.values[slot] = None;
                }
            }
        }
        Ok(())
    }
    fn commit(self: Box<Self>) -> StorageBackendResult<()> {
        Err(invalid(
            "native occurrence batches are committed by their enclosing operation",
        ))
    }
}
