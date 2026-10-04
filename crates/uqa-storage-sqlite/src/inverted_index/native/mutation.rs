//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Projected changes coalesce into complete native rows before the provider stages any result. After a whole-index replacement, rows arrive one at a time in ascending key order within each family, so each row is staged once the next row of its family arrives instead of every row being held until the batch ends.

mod encoding;

use super::{
    read::NativeRead,
    records::{self, invalid},
};
use crate::mvcc::native::{NativeRecordFamily as Family, NativeRecordOwner};
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
    /// After a whole-index replacement, the last row of each family.
    replacing: Vec<ReplacedRow>,
    /// Whether a whole-index replacement staged a row.
    replaced: bool,
}

/// The last row of a family a whole-index replacement changed, with the projections it still collects, or none once it is staged.
struct ReplacedRow {
    family: Family,
    key: BudgetedVec<u8>,
    draft: Option<Draft>,
}

/// Stage the native change `draft` makes to the row at `key`.
fn write_row(
    native: &mut dyn KeyValueBatch,
    read: &NativeRead,
    owner: NativeRecordOwner,
    key: &[u8],
    draft: &Draft,
) -> StorageBackendResult<()> {
    if draft.values.iter().all(Option::is_none) {
        if draft.merge {
            native.replace_occurrence_record(key, None)
        } else {
            native.delete(key)
        }
    } else {
        encoding::write(native, read, owner, draft)
    }
}

fn owned_value(
    value: Option<&[u8]>,
    control: &uqa_storage::read_control::StorageReadControl,
) -> StorageBackendResult<Option<BudgetedVec<u8>>> {
    value
        .map(|bytes| {
            let mut value = BudgetedVec::new(control.memory());
            value.extend_from_slice(bytes)?;
            Ok::<_, uqa_storage::StorageBackendError>(value)
        })
        .transpose()
}

impl<'a> ProjectionBatch<'a> {
    pub(super) fn new(read: &'a NativeRead, native: &'a mut dyn KeyValueBatch) -> Self {
        Self {
            read,
            native,
            owner: read.owner,
            rows: BTreeMap::new(),
            cleared: false,
            replacing: Vec::new(),
            replaced: false,
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
        let key = records::key(address, owner, control)?;
        if self.cleared {
            return self.replace_row(key, address, slot, value, merge);
        }
        let (key, key_memory) = key.into_parts();
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
        draft.values[slot] = owned_value(value, control)?;
        Ok(())
    }

    /// Change a row after a whole-index replacement, which writes each record once and the rows of a family in ascending key order. A row is staged as soon as its last projection arrives, from that value instead of a copy, and a row of a later key stages the row its family left incomplete; an earlier key, or a record of a staged row, could change a row already staged, so it is rejected.
    fn replace_row(
        &mut self,
        key: BudgetedVec<u8>,
        address: Address<'_>,
        slot: usize,
        value: Option<&[u8]>,
        merge: bool,
    ) -> StorageBackendResult<()> {
        let owner = self.ensure_owner()?;
        let family = records::family(address.projection.expect("complete address"))
            .expect("complete address has a family");
        let width = records::projections(family).len();
        let control = &self.read.snapshot.control;
        let position = self.replacing.iter().position(|row| row.family == family);
        if let Some(index) = position {
            let row = &mut self.replacing[index];
            match (*key).cmp(&*row.key) {
                std::cmp::Ordering::Equal => {
                    let Some(draft) = row.draft.as_mut() else {
                        return Err(invalid(
                            "a whole-index replacement writes each occurrence record once",
                        ));
                    };
                    draft.merge &= merge;
                    let mut values = [draft.values[0].as_deref(), draft.values[1].as_deref()];
                    values[slot] = value;
                    if values[..width].iter().all(Option::is_some) {
                        encoding::write_values(
                            self.native,
                            self.read,
                            owner,
                            &draft.address,
                            values,
                            draft.merge,
                        )?;
                        row.draft = None;
                        self.replaced = true;
                    } else {
                        draft.values[slot] = owned_value(value, control)?;
                    }
                    return Ok(());
                }
                std::cmp::Ordering::Less => {
                    return Err(invalid(
                        "rows a whole-index replacement writes must arrive in key order",
                    ));
                }
                std::cmp::Ordering::Greater => {
                    let finished = self.replacing.swap_remove(index);
                    if let Some(draft) = finished.draft {
                        write_row(self.native, self.read, owner, &finished.key, &draft)?;
                        self.replaced = true;
                    }
                }
            }
        }
        let encoded = address.encode(control)?;
        let draft = if width == 1 && value.is_some() {
            encoding::write_values(
                self.native,
                self.read,
                owner,
                &encoded,
                [value, None],
                merge,
            )?;
            self.replaced = true;
            None
        } else {
            let mut values = [None, None];
            values[slot] = owned_value(value, control)?;
            Some(Draft {
                address: encoded,
                values,
                _key: control.memory().empty_reservation(),
                _entry: control
                    .memory()
                    .reserve(std::mem::size_of::<ReplacedRow>())?,
                merge,
            })
        };
        self.replacing.push(ReplacedRow { family, key, draft });
        Ok(())
    }

    pub(super) fn flush(self) -> StorageBackendResult<()> {
        let Some(owner) = self.owner else {
            return Ok(());
        };
        let replacing = self
            .replacing
            .iter()
            .filter_map(|row| row.draft.as_ref().map(|draft| (&*row.key, draft)));
        for (key, draft) in self
            .rows
            .iter()
            .map(|(key, draft)| (key.as_slice(), draft))
            .chain(replacing)
        {
            self.read.snapshot.control.check()?;
            write_row(self.native, self.read, owner, key, draft)?;
        }
        Ok(())
    }
}

impl KeyValueBatch for ProjectionBatch<'_> {
    fn serializable_participant(&self) -> Option<uqa_storage::mvcc::SerializableTransactionId> {
        self.native.serializable_participant()
    }
    fn observe_serializable_write(
        &mut self,
        predicate: uqa_storage::mvcc::SerializablePredicate<'_>,
    ) -> StorageBackendResult<()> {
        self.native.observe_serializable_write(predicate)
    }
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
                self.replacing.clear();
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
        if self.replaced {
            return Err(invalid(
                "a whole-index replacement cannot delete part of the rows it staged",
            ));
        }
        if !self.cleared {
            for key in self.read.keys(prefix, None, usize::MAX, control)?.iter() {
                self.delete(key)?;
            }
        }
        // Include rows introduced earlier in this same callback, preserving delete-after-put order.
        for draft in self.rows.values_mut().chain(
            self.replacing
                .iter_mut()
                .filter_map(|row| row.draft.as_mut()),
        ) {
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
