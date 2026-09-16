//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Historical reads probe encoded lengths and reserve before materializing keys or values.

use rusqlite::{params, types::ValueRef, Connection, OptionalExtension};
use uqa_core::memory::BudgetedVec;
use uqa_storage::mvcc::{
    BorrowedRecord, CommitSequence, CommittedRecordSnapshot, RecordKeyVisitor, RecordMetadata,
    RecordPage, RecordScanVisitor, RecordValueVisitor, RecordVersion, ScannedRecord,
    SharedRecordValue, VersionError, VersionResult,
};
use uqa_storage::read_control::StorageReadControl;

use crate::read_control::{copy_bytes, payload_length, prefix_upper_bound, reserve_bindings};

use super::{codec, PhysicalResult, SQLiteRecordStore};

pub(super) struct Snapshot {
    pub(super) store: SQLiteRecordStore,
    pub(super) sequence: CommitSequence,
}

impl Snapshot {
    fn read<T>(
        &self,
        operation: impl FnOnce(&Connection) -> PhysicalResult<T>,
    ) -> VersionResult<T> {
        self.store.with(|connection| {
            let transaction = connection.unchecked_transaction()?;
            super::native::check_mapping(&transaction, self.store.native)?;
            codec::header(&transaction, self.store.identity)?;
            let result = operation(&transaction)?;
            transaction.commit()?;
            Ok(result)
        })
    }
}

impl CommittedRecordSnapshot for Snapshot {
    fn sequence(&self) -> CommitSequence {
        self.sequence
    }

    fn get(
        &self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Option<RecordVersion<SharedRecordValue>>> {
        let mut found = None;
        self.visit_value(key, control, &mut |record| {
            if let Some(record) = record {
                found = Some(RecordVersion::copy_bytes(
                    record.revision.expect("committed revision"),
                    record.value,
                    control,
                )?);
            }
            Ok(())
        })?;
        Ok(found)
    }

    fn scan(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
        control: &StorageReadControl,
    ) -> VersionResult<RecordPage> {
        let mut page = RecordPage::new(control.memory());
        self.visit_prefix(prefix, after, limit, control, &mut |key, record| {
            let version = RecordVersion::copy_bytes(
                record.revision.expect("committed revision"),
                record.value,
                control,
            )?;
            page.push(ScannedRecord::copy_key(key, version, control)?)?;
            Ok(true)
        })?;
        Ok(page)
    }

    fn visit_value(
        &self,
        key: &[u8],
        control: &StorageReadControl,
        visit: &mut RecordValueVisitor<'_>,
    ) -> VersionResult<()> {
        control.cancellation().check()?;
        self.read(|connection| value(connection, key, self.sequence, control, visit))
    }

    fn metadata(
        &self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Option<RecordMetadata>> {
        control.cancellation().check()?;
        self.read(|connection| {
            let _bindings = reserve_bindings(control, &[key])?;
            let record =
                info(connection, key, self.sequence)?.map(|(revision, length)| RecordMetadata {
                    revision: Some(CommitSequence::from_u64(revision)),
                    live: length.is_some(),
                });
            control.cancellation().check().map_err(VersionError::from)?;
            Ok(record)
        })
    }

    fn visit_prefix(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
        control: &StorageReadControl,
        visit: &mut RecordScanVisitor<'_>,
    ) -> VersionResult<()> {
        control.cancellation().check()?;
        if limit == 0 {
            return Ok(());
        }
        self.read(|connection| {
            keys(connection, prefix, after, limit, control, &mut |key| {
                let mut more = None;
                value(connection, key, self.sequence, control, &mut |record| {
                    if let Some(record) = record {
                        more = Some(visit(key, record)?);
                    }
                    Ok(())
                })?;
                Ok(more)
            })
        })
    }

    fn visit_keys(
        &self,
        prefix: &[u8],
        after: Option<&[u8]>,
        limit: usize,
        control: &StorageReadControl,
        visit: &mut RecordKeyVisitor<'_>,
    ) -> VersionResult<()> {
        control.cancellation().check()?;
        if limit == 0 {
            return Ok(());
        }
        self.read(|connection| {
            keys(connection, prefix, after, limit, control, &mut |key| {
                let _bindings = reserve_bindings(control, &[key])?;
                info(connection, key, self.sequence)?
                    .map(|(revision, length)| {
                        Ok(visit(
                            key,
                            RecordMetadata {
                                revision: Some(CommitSequence::from_u64(revision)),
                                live: length.is_some(),
                            },
                        )?)
                    })
                    .transpose()
            })
        })
    }
}

fn keys(
    connection: &Connection,
    prefix: &[u8],
    after: Option<&[u8]>,
    limit: usize,
    control: &StorageReadControl,
    visit: &mut impl FnMut(&[u8]) -> PhysicalResult<Option<bool>>,
) -> PhysicalResult<()> {
    let upper = prefix_upper_bound(prefix, control)?;
    let mut cursor: Option<BudgetedVec<u8>> = None;
    let mut count = 0;
    loop {
        let Some(key) = next_key(
            connection,
            prefix,
            cursor.as_deref().or(after),
            upper.as_deref(),
            control,
        )?
        else {
            break;
        };
        if let Some(more) = visit(&key)? {
            count += 1;
            control.cancellation().check().map_err(VersionError::from)?;
            if !more || count == limit {
                break;
            }
        }
        cursor = Some(key);
    }
    control.cancellation().check().map_err(VersionError::from)?;
    Ok(())
}

fn info(
    connection: &Connection,
    key: &[u8],
    boundary: CommitSequence,
) -> PhysicalResult<Option<(u64, Option<usize>)>> {
    let boundary = boundary.as_u64().to_be_bytes();
    let mut statement = connection.prepare("SELECT sequence, CASE WHEN value IS NULL THEN NULL WHEN typeof(value) = 'blob' THEN length(value) ELSE -1 END FROM _uqa_mvcc_versions WHERE key = ?1 AND sequence <= ?2 ORDER BY sequence DESC LIMIT 1")?;
    let mut rows = statement.query(params![key, boundary.as_slice()])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let revision = codec::integer(codec::bytes(row, 0)?)?;
    if revision == 0 {
        return Err(VersionError::InvalidEncoding("zero record revision").into());
    }
    Ok(Some((
        revision,
        row.get::<_, Option<i64>>(1)?
            .map(payload_length)
            .transpose()?,
    )))
}

pub(super) fn value(
    connection: &Connection,
    key: &[u8],
    boundary: CommitSequence,
    control: &StorageReadControl,
    visit: &mut RecordValueVisitor<'_>,
) -> PhysicalResult<()> {
    let _bindings = reserve_bindings(control, &[key])?;
    let info = info(connection, key, boundary)?;
    let Some((revision, length)) = info else {
        control.cancellation().check().map_err(VersionError::from)?;
        visit(None)?;
        control.cancellation().check().map_err(VersionError::from)?;
        return Ok(());
    };
    control.cancellation().check().map_err(VersionError::from)?;
    let _payload = control
        .memory()
        .reserve(length.unwrap_or(0))
        .map_err(VersionError::from)?;
    let mut statement = connection
        .prepare("SELECT value FROM _uqa_mvcc_versions WHERE key = ?1 AND sequence = ?2")?;
    let revision_bytes = revision.to_be_bytes();
    let mut rows = statement.query(params![key, revision_bytes.as_slice()])?;
    let row = rows.next()?.ok_or(VersionError::InvalidEncoding(
        "version disappeared within a read",
    ))?;
    let value = match row.get_ref(0)? {
        ValueRef::Null if length.is_none() => None,
        ValueRef::Blob(bytes) if length == Some(bytes.len()) => Some(bytes),
        _ => {
            return Err(
                VersionError::InvalidEncoding("version size or type changed within a read").into(),
            )
        }
    };
    control.cancellation().check().map_err(VersionError::from)?;
    visit(Some(BorrowedRecord {
        revision: Some(CommitSequence::from_u64(revision)),
        value,
    }))?;
    control.cancellation().check().map_err(VersionError::from)?;
    Ok(())
}

fn next_key(
    connection: &Connection,
    prefix: &[u8],
    after: Option<&[u8]>,
    upper: Option<&[u8]>,
    control: &StorageReadControl,
) -> PhysicalResult<Option<BudgetedVec<u8>>> {
    control.cancellation().check().map_err(VersionError::from)?;
    let after = after.filter(|after| *after >= prefix);
    let lower = after.unwrap_or(prefix);
    let _bindings = reserve_bindings(control, &[lower, upper.unwrap_or_default()])?;
    let (sizes, data) = match (after.is_some(), upper.is_some()) {
        (true, true) => ("SELECT length(key) FROM _uqa_mvcc_heads WHERE key > ?1 AND key < ?2 ORDER BY key LIMIT 1", "SELECT key FROM _uqa_mvcc_heads WHERE key > ?1 AND key < ?2 ORDER BY key LIMIT 1"),
        (true, false) => ("SELECT length(key) FROM _uqa_mvcc_heads WHERE key > ?1 AND (?2 IS NULL) ORDER BY key LIMIT 1", "SELECT key FROM _uqa_mvcc_heads WHERE key > ?1 AND (?2 IS NULL) ORDER BY key LIMIT 1"),
        (false, true) => ("SELECT length(key) FROM _uqa_mvcc_heads WHERE key >= ?1 AND key < ?2 ORDER BY key LIMIT 1", "SELECT key FROM _uqa_mvcc_heads WHERE key >= ?1 AND key < ?2 ORDER BY key LIMIT 1"),
        (false, false) => ("SELECT length(key) FROM _uqa_mvcc_heads WHERE key >= ?1 AND (?2 IS NULL) ORDER BY key LIMIT 1", "SELECT key FROM _uqa_mvcc_heads WHERE key >= ?1 AND (?2 IS NULL) ORDER BY key LIMIT 1"),
    };
    let length: Option<i64> = connection
        .query_row(sizes, params![lower, upper], |row| row.get(0))
        .optional()?;
    let Some(length) = length else {
        return Ok(None);
    };
    let length = payload_length(length)?;
    control.cancellation().check().map_err(VersionError::from)?;
    let _payload = control
        .memory()
        .reserve(length)
        .map_err(VersionError::from)?;
    let mut statement = connection.prepare(data)?;
    let mut rows = statement.query(params![lower, upper])?;
    let row = rows.next()?.ok_or(VersionError::InvalidEncoding(
        "head disappeared within a read",
    ))?;
    let bytes = codec::bytes(row, 0)?;
    if bytes.len() != length || !bytes.starts_with(prefix) {
        return Err(VersionError::InvalidEncoding("head changed within a read").into());
    }
    Ok(Some(copy_bytes(bytes, 0, control)?))
}
