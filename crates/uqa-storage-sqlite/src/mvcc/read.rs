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

use super::{codec, runs, PhysicalResult, SQLiteRecordStore};

pub(super) struct Snapshot {
    pub(super) store: SQLiteRecordStore,
    pub(super) sequence: CommitSequence,
    pub(super) _lease: std::sync::Arc<uqa_storage::mvcc::SnapshotLease>,
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

    fn visit_value_bounded(
        &self,
        key: &[u8],
        max_bytes: usize,
        control: &StorageReadControl,
        visit: &mut RecordValueVisitor<'_>,
    ) -> VersionResult<()> {
        control.cancellation().check()?;
        self.read(|connection| {
            value_bounded(connection, key, self.sequence, max_bytes, control, visit)
        })
    }

    fn metadata(
        &self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Option<RecordMetadata>> {
        control.cancellation().check()?;
        self.read(|connection| {
            let _bindings = reserve_bindings(control, &[key])?;
            let record = info(connection, key, self.sequence)?.map(|info| RecordMetadata {
                revision: Some(CommitSequence::from_u64(info.revision)),
                live: info.length.is_some(),
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
                    .map(|info| {
                        Ok(visit(
                            key,
                            RecordMetadata {
                                revision: Some(CommitSequence::from_u64(info.revision)),
                                live: info.length.is_some(),
                            },
                        )?)
                    })
                    .transpose()
            })
        })
    }
}

pub(super) fn keys(
    connection: &Connection,
    prefix: &[u8],
    after: Option<&[u8]>,
    limit: usize,
    control: &StorageReadControl,
    visit: &mut impl FnMut(&[u8]) -> PhysicalResult<Option<bool>>,
) -> PhysicalResult<()> {
    let upper = prefix_upper_bound(prefix, control)?;
    let has_runs: bool =
        connection.query_row("SELECT EXISTS(SELECT 1 FROM _uqa_mvcc_runs)", [], |row| {
            row.get(0)
        })?;
    walk_keys(
        after,
        limit,
        control,
        &mut |after| {
            let point = next_key(connection, prefix, after, upper.as_deref(), control)?;
            if !has_runs {
                return Ok(point);
            }
            let after = after.filter(|after| *after >= prefix);
            let run = runs::next_key(
                connection,
                after.unwrap_or(prefix),
                after.is_some(),
                point.as_deref().or(upper.as_deref()),
                control,
            )?;
            Ok(match (point, run) {
                (Some(point), Some(run)) if point[..] <= run[..] => Some(point),
                (_, Some(run)) => Some(run),
                (point, None) => point,
            })
        },
        visit,
    )
}

pub(super) fn point_keys(
    connection: &Connection,
    prefix: &[u8],
    after: Option<&[u8]>,
    limit: usize,
    control: &StorageReadControl,
    visit: &mut impl FnMut(&[u8]) -> PhysicalResult<Option<bool>>,
) -> PhysicalResult<()> {
    let upper = prefix_upper_bound(prefix, control)?;
    walk_keys(
        after,
        limit,
        control,
        &mut |after| next_key(connection, prefix, after, upper.as_deref(), control),
        visit,
    )
}

fn walk_keys(
    after: Option<&[u8]>,
    limit: usize,
    control: &StorageReadControl,
    next: &mut impl FnMut(Option<&[u8]>) -> PhysicalResult<Option<BudgetedVec<u8>>>,
    visit: &mut impl FnMut(&[u8]) -> PhysicalResult<Option<bool>>,
) -> PhysicalResult<()> {
    if limit == 0 {
        return Ok(());
    }
    let mut cursor: Option<BudgetedVec<u8>> = None;
    let mut count = 0;
    loop {
        let Some(key) = next(cursor.as_deref().or(after))? else {
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

pub(super) struct Info {
    pub(super) revision: u64,
    pub(super) length: Option<usize>,
    run: bool,
}

pub(super) fn info(
    connection: &Connection,
    key: &[u8],
    boundary: CommitSequence,
) -> PhysicalResult<Option<Info>> {
    let mut statement = connection.prepare("SELECT h.sequence, h.compacted, v.sequence, CASE WHEN v.value IS NULL THEN NULL WHEN typeof(v.value) = 'blob' THEN length(v.value) ELSE -1 END FROM _uqa_mvcc_heads h LEFT JOIN _uqa_mvcc_versions v ON v.key = h.key AND v.sequence = (SELECT sequence FROM _uqa_mvcc_versions WHERE key = ?1 AND sequence <= ?2 ORDER BY sequence DESC LIMIT 1) WHERE h.key = ?1")?;
    let mut rows = statement.query(params![key, boundary.as_u64().to_be_bytes().as_slice()])?;
    let Some(row) = rows.next()? else {
        return Ok(runs::info(connection, key)?
            .filter(|(sequence, _)| *sequence <= boundary)
            .map(|(sequence, length)| Info {
                revision: sequence.as_u64(),
                length,
                run: true,
            }));
    };
    let (head, compacted) = codec::decode_head(row)?;
    let head_visible = head <= boundary;
    if head_visible && compacted {
        return Ok(Some(Info {
            revision: head.as_u64(),
            length: None,
            run: false,
        }));
    }
    if matches!(row.get_ref(2)?, ValueRef::Null) {
        if head_visible {
            return Err(VersionError::InvalidEncoding("record head has no version").into());
        }
        return Ok(None);
    }
    let revision = codec::integer(codec::bytes(row, 2)?)?;
    if head_visible && head.as_u64() != revision {
        return Err(VersionError::InvalidEncoding("record head has no matching version").into());
    }
    if revision == 0 {
        return Err(VersionError::InvalidEncoding("zero record revision").into());
    }
    Ok(Some(Info {
        revision,
        length: row
            .get::<_, Option<i64>>(3)?
            .map(payload_length)
            .transpose()?,
        run: false,
    }))
}

pub(super) fn value(
    connection: &Connection,
    key: &[u8],
    boundary: CommitSequence,
    control: &StorageReadControl,
    visit: &mut RecordValueVisitor<'_>,
) -> PhysicalResult<()> {
    value_bounded(connection, key, boundary, usize::MAX, control, visit)
}

fn value_bounded(
    connection: &Connection,
    key: &[u8],
    boundary: CommitSequence,
    max_bytes: usize,
    control: &StorageReadControl,
    visit: &mut RecordValueVisitor<'_>,
) -> PhysicalResult<()> {
    let _bindings = reserve_bindings(control, &[key])?;
    let info = info(connection, key, boundary)?;
    let Some(info) = info else {
        control.cancellation().check().map_err(VersionError::from)?;
        visit(None)?;
        control.cancellation().check().map_err(VersionError::from)?;
        return Ok(());
    };
    control
        .check_value_size(info.length.unwrap_or(0), max_bytes)
        .map_err(VersionError::from)?;
    if info.run {
        return runs::value(connection, key, boundary, control, visit);
    }
    let Info {
        revision, length, ..
    } = info;
    control.cancellation().check().map_err(VersionError::from)?;
    if length.is_none() {
        visit(Some(BorrowedRecord {
            revision: Some(CommitSequence::from_u64(revision)),
            value: None,
        }))?;
        control.cancellation().check().map_err(VersionError::from)?;
        return Ok(());
    }
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
