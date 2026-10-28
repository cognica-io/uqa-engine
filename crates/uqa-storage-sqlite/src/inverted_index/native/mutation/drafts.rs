//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Coalesced native rows retain a bounded prefix and spill through the common encrypted record owner.

use super::{invalid, Draft};
use uqa_core::memory::BudgetedVec;
use uqa_storage::mvcc::{PrivateRecordChanges, PrivateRecordSnapshot, RecordWrite, VersionError};
use uqa_storage::read_control::StorageReadControl;
use uqa_storage::StorageBackendResult;

pub(super) struct Drafts {
    records: PrivateRecordChanges,
    control: StorageReadControl,
    // Consecutive projections of one row coalesce before encoding or spilling it.
    pending: Option<(BudgetedVec<u8>, Draft)>,
}

impl Drafts {
    pub(super) fn new(control: &StorageReadControl) -> Self {
        Self {
            // Prefix edits may retain the old root while writing its replacement. Leave room for both roots, a decoded row, and the enclosing transaction's evaluated records.
            records: PrivateRecordChanges::with_memory_tier_limit(
                control.memory(),
                control.memory().limit() / 16,
            ),
            control: control.clone(),
            pending: None,
        }
    }

    pub(super) fn clear(&mut self) {
        *self = Self::new(&self.control);
    }

    /// Transfer one row into a provisional edit. A failed edit abandons the enclosing operation and all of its drafts.
    pub(super) fn take_for_update(&mut self, key: &[u8]) -> StorageBackendResult<Option<Draft>> {
        self.control.check()?;
        if self
            .pending
            .as_ref()
            .is_some_and(|(pending, _)| &pending[..] == key)
        {
            return Ok(self.pending.take().map(|(_, draft)| draft));
        }
        self.flush_pending()?;
        self.get_stored(key)
    }

    #[cfg(test)]
    fn get(&mut self, key: &[u8]) -> StorageBackendResult<Option<Draft>> {
        self.flush_pending()?;
        self.get_stored(key)
    }

    fn get_stored(&self, key: &[u8]) -> StorageBackendResult<Option<Draft>> {
        self.records
            .get(key, &self.control)
            .map_err(VersionError::into_storage_error)?
            .map(|record| {
                decode(
                    record
                        .value()
                        .ok_or_else(|| invalid("missing staged native row"))?,
                    &self.control,
                )
            })
            .transpose()
    }

    pub(super) fn put(&mut self, key: &[u8], draft: Draft) -> StorageBackendResult<()> {
        self.control.check()?;
        if self
            .pending
            .as_ref()
            .is_some_and(|(pending, _)| &pending[..] != key)
        {
            self.flush_pending()?;
        }
        let mut owned_key = BudgetedVec::new(self.control.memory());
        owned_key.extend_from_slice(key)?;
        self.pending = Some((owned_key, draft));
        Ok(())
    }

    fn flush_pending(&mut self) -> StorageBackendResult<()> {
        let Some((key, draft)) = self.pending.take() else {
            return Ok(());
        };
        let bytes = std::iter::once(&draft.address[..])
            .chain(draft.values.iter().filter_map(|value| value.as_deref()))
            .try_fold(1 + 3 * size_of::<u64>(), |bytes, value| {
                bytes
                    .checked_add(value.len())
                    .ok_or(uqa_core::memory::MemoryError::SizeOverflow)
            })?;
        let mut encoded = BudgetedVec::new(self.control.memory());
        encoded.reserve(bytes)?;
        encoded.push(u8::from(draft.merge))?;
        append(&mut encoded, Some(&draft.address))?;
        for value in &draft.values {
            append(&mut encoded, value.as_deref())?;
        }
        // The encoded row replaces its projection buffers before the record owner captures it.
        drop(draft);
        self.records
            .apply(
                &[RecordWrite {
                    key: &key,
                    expected: None,
                    value: Some(&encoded),
                }],
                &self.control,
            )
            .map_err(VersionError::into_storage_error)?;
        Ok(())
    }

    pub(super) fn visit(
        &mut self,
        visit: impl FnMut(&[u8], Draft) -> StorageBackendResult<()>,
    ) -> StorageBackendResult<()> {
        self.flush_pending()?;
        visit_records(
            &self
                .records
                .snapshot()
                .map_err(VersionError::into_storage_error)?,
            &self.control,
            visit,
        )
    }

    pub(super) fn remove_prefix(&mut self, prefix: &[u8]) -> StorageBackendResult<()> {
        self.flush_pending()?;
        let snapshot = self
            .records
            .snapshot()
            .map_err(VersionError::into_storage_error)?;
        let control = self.control.clone();
        visit_records(&snapshot, &control, |key, mut draft| {
            if draft.remove_prefix(prefix, &control)? {
                self.put(key, draft)?;
            }
            Ok(())
        })
    }
}

fn visit_records(
    records: &PrivateRecordSnapshot,
    control: &StorageReadControl,
    mut visit: impl FnMut(&[u8], Draft) -> StorageBackendResult<()>,
) -> StorageBackendResult<()> {
    records
        .visit(b"", None, control, &mut |record| {
            let draft = decode(
                record
                    .value()
                    .ok_or_else(|| invalid("missing staged native row"))?,
                control,
            )?;
            visit(record.key(), draft).map_err(VersionError::from)?;
            Ok(true)
        })
        .map_err(VersionError::into_storage_error)
}

fn append(output: &mut BudgetedVec<u8>, value: Option<&[u8]>) -> StorageBackendResult<()> {
    let length = value.map_or(Ok(u64::MAX), |value| {
        u64::try_from(value.len()).map_err(|_| uqa_core::memory::MemoryError::SizeOverflow)
    })?;
    output.extend_from_slice(&length.to_le_bytes())?;
    if let Some(value) = value {
        output.extend_from_slice(value)?;
    }
    Ok(())
}

fn take<'a>(input: &mut &'a [u8]) -> StorageBackendResult<Option<&'a [u8]>> {
    let (header, rest) = input
        .split_at_checked(8)
        .ok_or_else(|| invalid("short staged native row length"))?;
    let length = u64::from_le_bytes(header.try_into().expect("eight-byte length"));
    if length == u64::MAX {
        *input = rest;
        return Ok(None);
    }
    let length =
        usize::try_from(length).map_err(|_| invalid("staged native row length overflow"))?;
    let (value, rest) = rest
        .split_at_checked(length)
        .ok_or_else(|| invalid("short staged native row value"))?;
    *input = rest;
    Ok(Some(value))
}

fn decode(mut input: &[u8], control: &StorageReadControl) -> StorageBackendResult<Draft> {
    control.check()?;
    let (&merge, rest) = input
        .split_first()
        .ok_or_else(|| invalid("empty staged native row"))?;
    if merge > 1 {
        return Err(invalid("invalid staged native row merge flag"));
    }
    input = rest;
    let address = take(&mut input)?.ok_or_else(|| invalid("missing staged native row address"))?;
    let mut owned_address = BudgetedVec::new(control.memory());
    owned_address.extend_from_slice(address)?;
    let values = [
        super::owned_value(take(&mut input)?, control)?,
        super::owned_value(take(&mut input)?, control)?,
    ];
    if !input.is_empty() {
        return Err(invalid("trailing staged native row data"));
    }
    Ok(Draft {
        address: owned_address,
        values,
        _key: control.memory().empty_reservation(),
        _entry: control.memory().empty_reservation(),
        merge: merge != 0,
    })
}

#[cfg(test)]
mod tests;
