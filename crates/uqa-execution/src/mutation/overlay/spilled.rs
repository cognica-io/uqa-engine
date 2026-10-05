//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The rows a command staged for a table and then moved out of memory, kept as private records that spill to encrypted temporary files as a transaction's own changes do.
//!
//! A row is the record `ROW` followed by its identity in big-endian order, whose value is the row's tuple xmin and fields, or no value for a row the command deleted. An exact index entry is the record `INDEX`, the index's ordinal, the length of the row's canonical key and the key, followed by the row's identity, whose value records whether the row holds every field of the index; an entry the command removed has no value. Identities in big-endian order keep both kinds of records in identity order. A key record is the record `KEY`, the index's ordinal, the key's length and the key, whose value counts the key's entries; a probe of a key that no spilled row holds reads only that record, which the spilled runs' key filters answer without reading a run.

use std::sync::Arc;

use uqa_core::{memory::BudgetedVec, DocId};
use uqa_storage::document_store::DocumentMetadata;
use uqa_storage::mvcc::{PrivateRecordChanges, PrivateRecordSnapshot, RecordWrite, VersionError};
use uqa_storage::read_control::StorageReadControl;
use uqa_storage::{StorageBackendError, StorageBackendResult, StorageSavepointId};

use super::CommandStoredDocument;
use uqa_sql::SQLError;

const ROW: u8 = 0;
const INDEX: u8 = 1;
const KEY: u8 = 2;
/// The bytes of a row record's key.
const ROW_KEY_BYTES: usize = 1 + size_of::<u64>();
/// The records a page of a scan keeps at most.
pub(super) const PAGE_RECORDS: usize = 256;
/// The bytes of keys and values after which a page of a scan ends; a page keeps at least one record.
const PAGE_BYTES: usize = 256 * 1024;

/// A staged row: its fields, or `None` for a row the command deleted.
pub(crate) type StagedRow = Option<CommandStoredDocument>;

/// One record to write: its key, and its value or `None` to remove it.
pub(super) type SpilledRecord = (Vec<u8>, Option<Vec<u8>>);

/// The key of the row record of `id`.
pub(super) fn row_key(id: DocId) -> Vec<u8> {
    let mut key = Vec::with_capacity(ROW_KEY_BYTES);
    key.push(ROW);
    key.extend_from_slice(&id.to_be_bytes());
    key
}

/// The identity at the end of a record key.
fn key_identity(key: &[u8]) -> StorageBackendResult<DocId> {
    key.len()
        .checked_sub(size_of::<u64>())
        .and_then(|start| key[start..].try_into().ok())
        .map(DocId::from_be_bytes)
        .ok_or_else(|| invalid("a spilled command record lacks its row identity"))
}

fn invalid(message: &str) -> StorageBackendError {
    StorageBackendError::Other(message.into())
}

/// The record tag `tag` followed by exact index `ordinal` and canonical key `key`.
fn keyed(tag: u8, ordinal: u32, key: &[u8]) -> StorageBackendResult<Vec<u8>> {
    let length =
        u32::try_from(key.len()).map_err(|_| invalid("a command exact key exceeds 4 GiB"))?;
    let mut prefix = Vec::new();
    prefix
        .try_reserve_exact(1 + 2 * size_of::<u32>() + key.len() + size_of::<u64>())
        .map_err(|error| {
            StorageBackendError::Other(format!("allocate a spilled index key: {error}"))
        })?;
    prefix.push(tag);
    prefix.extend_from_slice(&ordinal.to_be_bytes());
    prefix.extend_from_slice(&length.to_be_bytes());
    prefix.extend_from_slice(key);
    Ok(prefix)
}

/// The prefix of the entries of exact index `ordinal` for canonical key `key`.
fn index_prefix(ordinal: u32, key: &[u8]) -> StorageBackendResult<Vec<u8>> {
    keyed(INDEX, ordinal, key)
}

/// The key record of exact index `ordinal` for canonical key `key`, which counts the key's entries.
pub(super) fn key_record(ordinal: u32, key: &[u8]) -> StorageBackendResult<Vec<u8>> {
    keyed(KEY, ordinal, key)
}

/// The key of the entry of exact index `ordinal` for row `id` under canonical key `key`.
pub(super) fn index_key(ordinal: u32, key: &[u8], id: DocId) -> StorageBackendResult<Vec<u8>> {
    let mut entry = index_prefix(ordinal, key)?;
    entry.extend_from_slice(&id.to_be_bytes());
    Ok(entry)
}

/// The value of an index entry for a row that holds every field of the index, or not.
pub(super) fn index_value(complete: bool) -> Vec<u8> {
    vec![u8::from(complete)]
}

/// The value of a row record: its tuple xmin and its fields.
pub(super) fn encode_row(row: &CommandStoredDocument) -> StorageBackendResult<Vec<u8>> {
    let fields = row.fields.as_ref();
    let bytes = crate::spill::encoded_document_size(fields)
        .map_err(exec_error)?
        .checked_add(1 + size_of::<u32>())
        .ok_or_else(|| invalid("spilled command row size overflow"))?;
    let mut value = Vec::new();
    value.try_reserve_exact(bytes).map_err(|error| {
        StorageBackendError::Other(format!("allocate a spilled command row: {error}"))
    })?;
    match row.metadata.tuple_xmin() {
        Some(xmin) => {
            value.push(1);
            value.extend_from_slice(&xmin.to_le_bytes());
        }
        None => value.extend_from_slice(&[0; 1 + size_of::<u32>()]),
    }
    value[0] |= u8::from(row.published) << 1;
    crate::spill::encode_document(&mut value, fields).map_err(exec_error)?;
    Ok(value)
}

/// The row a row record's value holds, charged to `control`.
fn decode_row(
    value: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<CommandStoredDocument> {
    let (header, document) = value
        .split_at_checked(1 + size_of::<u32>())
        .ok_or_else(|| invalid("a spilled command row lacks its header"))?;
    if header[0] > 3 {
        return Err(invalid("a spilled command row has invalid flags"));
    }
    let metadata = match header[0] & 1 {
        0 => DocumentMetadata::default(),
        1 => DocumentMetadata::with_tuple_xmin(u32::from_le_bytes(
            header[1..].try_into().expect("four xmin bytes"),
        )),
        _ => return Err(invalid("a spilled command row has an invalid xmin flag")),
    };
    let (fields, rest) = crate::spill::decode_document(document).map_err(exec_error)?;
    if !rest.is_empty() {
        return Err(invalid("a spilled command row has trailing bytes"));
    }
    let mut row = CommandStoredDocument::new(Arc::new(fields), metadata, control)?;
    row.published = header[0] & 2 != 0;
    Ok(row)
}

fn exec_error(error: crate::ExecError) -> StorageBackendError {
    StorageBackendError::Other(error.to_string())
}

fn version_error(error: VersionError) -> StorageBackendError {
    error.into_storage_error()
}

/// The spilled rows of one table of one command, with the view that reads see.
pub(super) struct SpilledRows {
    records: PrivateRecordChanges,
    view: Arc<PrivateRecordSnapshot>,
}

impl Clone for SpilledRows {
    fn clone(&self) -> Self {
        Self {
            records: self.records.fork(),
            view: Arc::clone(&self.view),
        }
    }
}

impl SpilledRows {
    pub(super) fn new(control: &StorageReadControl) -> StorageBackendResult<Self> {
        let records = PrivateRecordChanges::new(control.memory());
        let view = Arc::new(records.snapshot().map_err(version_error)?);
        Ok(Self { records, view })
    }

    /// The immutable view of the records written so far.
    pub(super) fn view(&self) -> &Arc<PrivateRecordSnapshot> {
        &self.view
    }

    /// Write the record batches that `change` passes to its writer, then let reads see them. When `change` or a batch fails, the records and the view stay as they were.
    pub(super) fn transact(
        &mut self,
        control: &StorageReadControl,
        change: impl FnOnce(&SpillWriter<'_>) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        let savepoint = StorageSavepointId::allocate();
        self.records
            .savepoint(savepoint)
            .map_err(|error| super::storage_error(version_error(error)))?;
        let written = change(&SpillWriter {
            records: &self.records,
            control,
        });
        let finished = match written {
            // Restoring the retained root does not allocate; the savepoint is known to exist.
            Err(error) => {
                return self
                    .records
                    .rollback_to_savepoint(savepoint)
                    .and_then(|()| self.records.release_savepoint(savepoint))
                    .map_err(|undo| super::storage_error(version_error(undo)))
                    .and(Err(error));
            }
            Ok(()) => self
                .records
                .release_savepoint(savepoint)
                .and_then(|()| self.records.snapshot()),
        };
        self.view = Arc::new(finished.map_err(|error| super::storage_error(version_error(error)))?);
        Ok(())
    }
}

/// Writes record batches of one [`SpilledRows::transact`].
pub(super) struct SpillWriter<'a> {
    records: &'a PrivateRecordChanges,
    control: &'a StorageReadControl,
}

impl SpillWriter<'_> {
    /// Write `batch`, whose records have distinct keys, together with the key records whose entry counts `changes` moves, and empty `batch` and `changes`.
    pub(super) fn write(
        &self,
        batch: &mut Vec<SpilledRecord>,
        changes: &mut std::collections::BTreeMap<Vec<u8>, i64>,
    ) -> Result<(), SQLError> {
        if !changes.is_empty() {
            // The counts so far include the batches this transaction already wrote.
            let view = self
                .records
                .snapshot()
                .map_err(|error| super::storage_error(version_error(error)))?;
            for (key, change) in std::mem::take(changes) {
                if change == 0 {
                    continue;
                }
                let count = entry_count(&view, &key, self.control)
                    .map_err(super::storage_error)?
                    .checked_add_signed(change)
                    .ok_or_else(|| {
                        SQLError::Internal("a spilled key count is out of range".into())
                    })?;
                batch.push((key, (count != 0).then(|| count.to_le_bytes().to_vec())));
            }
        }
        if batch.is_empty() {
            return Ok(());
        }
        let writes = batch
            .iter()
            .map(|(key, value)| RecordWrite {
                key,
                expected: None,
                value: value.as_deref(),
            })
            .collect::<Vec<_>>();
        self.records
            .apply(&writes, self.control)
            .map_err(|error| super::storage_error(version_error(error)))?;
        batch.clear();
        Ok(())
    }
}

/// The number of entries the key record `key` counts in a view.
fn entry_count(
    view: &PrivateRecordSnapshot,
    key: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<u64> {
    let Some(write) = view.get(key, control).map_err(version_error)? else {
        return Ok(0);
    };
    let Some(value) = write.value() else {
        return Ok(0);
    };
    value
        .try_into()
        .map(u64::from_le_bytes)
        .map_err(|_| invalid("a spilled key count is malformed"))
}

/// The row record of `id` in a view of spilled rows.
pub(super) fn row(
    view: &PrivateRecordSnapshot,
    id: DocId,
    control: &StorageReadControl,
) -> StorageBackendResult<Option<StagedRow>> {
    view.get(&row_key(id), control)
        .map_err(version_error)?
        .map(|write| {
            write
                .value()
                .map(|value| decode_row(value, control))
                .transpose()
        })
        .transpose()
}

/// Whether a view of spilled rows holds a row record for `id`, without decoding the row.
pub(super) fn holds_row(
    view: &PrivateRecordSnapshot,
    id: DocId,
    control: &StorageReadControl,
) -> StorageBackendResult<bool> {
    Ok(view
        .get(&row_key(id), control)
        .map_err(version_error)?
        .is_some())
}

/// A page of row records in identity order.
pub(super) struct RowPage<T> {
    pub(super) rows: BudgetedVec<(DocId, T)>,
    /// The identity to continue after, when rows may follow.
    pub(super) resume: Option<DocId>,
}

/// A bounded page of the row records of a view after identity `after`, with their decoded rows.
pub(super) fn row_page(
    view: &PrivateRecordSnapshot,
    after: Option<DocId>,
    control: &StorageReadControl,
) -> StorageBackendResult<RowPage<StagedRow>> {
    scan_rows(view, after, control, |value| {
        value.map(|value| decode_row(value, control)).transpose()
    })
}

/// A bounded page of the row records of a view after identity `after`, with whether each row is present.
pub(super) fn presence_page(
    view: &PrivateRecordSnapshot,
    after: Option<DocId>,
    control: &StorageReadControl,
) -> StorageBackendResult<RowPage<bool>> {
    scan_rows(view, after, control, |value| Ok(value.is_some()))
}

fn scan_rows<T>(
    view: &PrivateRecordSnapshot,
    after: Option<DocId>,
    control: &StorageReadControl,
    mut read: impl FnMut(Option<&[u8]>) -> StorageBackendResult<T>,
) -> StorageBackendResult<RowPage<T>> {
    let after = after.map(row_key);
    let mut rows = BudgetedVec::new(control.memory());
    let mut page = PageBound::default();
    view.visit(&[ROW], after.as_deref(), control, &mut |write| {
        let id = key_identity(write.key()).map_err(VersionError::from)?;
        rows.push((id, read(write.value()).map_err(VersionError::from)?))?;
        Ok(page.admit(write))
    })
    .map_err(version_error)?;
    let resume = rows.last().map(|(id, _)| *id).filter(|_| page.full);
    Ok(RowPage { rows, resume })
}

/// The records and bytes a page has kept, which end it at [`PAGE_RECORDS`] records or [`PAGE_BYTES`] bytes.
#[derive(Default)]
struct PageBound {
    records: usize,
    bytes: usize,
    full: bool,
}

impl PageBound {
    /// Count a kept record; whether the page takes another.
    fn admit(&mut self, write: &uqa_storage::mvcc::PreparedRecordWrite) -> bool {
        self.records += 1;
        self.bytes = self
            .bytes
            .saturating_add(write.key().len())
            .saturating_add(write.value().map_or(0, <[u8]>::len));
        self.full = self.records == PAGE_RECORDS || self.bytes >= PAGE_BYTES;
        !self.full
    }
}

/// A bounded page of the entries of a view's exact index `ordinal` for canonical key `key` after identity `after`, each with whether its row holds every field of the index.
pub(super) fn index_page(
    view: &PrivateRecordSnapshot,
    ordinal: u32,
    key: &[u8],
    after: Option<DocId>,
    control: &StorageReadControl,
) -> StorageBackendResult<RowPage<bool>> {
    let mut rows = BudgetedVec::new(control.memory());
    if after.is_none() && entry_count(view, &key_record(ordinal, key)?, control)? == 0 {
        return Ok(RowPage { rows, resume: None });
    }
    let prefix = index_prefix(ordinal, key)?;
    let after = after.map(|id| index_key(ordinal, key, id)).transpose()?;
    let mut page = PageBound::default();
    let mut last = None;
    view.visit(&prefix, after.as_deref(), control, &mut |write| {
        let id = key_identity(write.key()).map_err(VersionError::from)?;
        last = Some(id);
        // A removed entry has no value.
        if let Some(value) = write.value() {
            rows.push((id, value.first() == Some(&1)))?;
        }
        Ok(page.admit(write))
    })
    .map_err(version_error)?;
    Ok(RowPage {
        rows,
        resume: last.filter(|_| page.full),
    })
}
