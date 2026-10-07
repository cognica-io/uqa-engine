//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Historical reads probe encoded lengths and reserve before materializing keys or values.

mod last;
mod ordered;
pub(crate) mod row_presence;
pub(crate) mod table_owners;
mod values;

use rusqlite::{params, types::ValueRef, Connection, OptionalExtension};
use uqa_core::memory::BudgetedVec;
use uqa_storage::mvcc::{
    BorrowedRecord, CommitSequence, CommittedRecordSnapshot, RecordKeyIterator, RecordKeyVisitor,
    RecordMetadata, RecordPage, RecordPointVisitor, RecordScanVisitor, RecordValueVisitor,
    RecordVersion, ScannedRecord, SharedRecordValue, VersionError, VersionResult,
};
use uqa_storage::read_control::StorageReadControl;

use crate::read_control::{copy_bytes, payload_length, prefix_upper_bound, reserve_bindings};

use super::{codec, runs, PhysicalResult, SQLiteRecordStore};

pub(super) struct Snapshot {
    pub(super) store: SQLiteRecordStore,
    pub(super) sequence: CommitSequence,
    pub(super) reclamation_epoch: u64,
    /// The commit monitor's value before this snapshot, or a later one of the same sequence, was captured.
    pub(super) monitor: Option<std::sync::atomic::AtomicU64>,
    pub(crate) table_owners: table_owners::TableOwners,
    pub(crate) row_presence: row_presence::RowPresence,
    pub(super) _lease: std::sync::Arc<uqa_storage::mvcc::SnapshotLease>,
}

impl Snapshot {
    fn read<T>(
        &self,
        operation: impl FnOnce(&Connection) -> PhysicalResult<T>,
    ) -> VersionResult<T> {
        self.store.read(operation)
    }

    /// Run `operation` in one physical read when this snapshot's boundary is the database's latest commit, so the native projections, which each commit materializes in its own transaction, hold exactly this snapshot's committed records. Returns `None` when a newer commit exists.
    pub(super) fn read_latest<T>(
        &self,
        operation: impl FnOnce(&Connection) -> PhysicalResult<Option<T>>,
    ) -> VersionResult<Option<T>> {
        self.read(|connection| {
            if codec::header(connection, self.store.identity)?.sequence != self.sequence {
                return Ok(None);
            }
            operation(connection)
        })
    }

    /// Read the current projection and retained history in one physical view.
    /// The caller must reconcile changes newer than this snapshot's boundary.
    pub(super) fn read_historical<T>(
        &self,
        operation: impl FnOnce(&Connection) -> PhysicalResult<Option<T>>,
    ) -> VersionResult<Option<T>> {
        self.read(|connection| {
            if codec::header(connection, self.store.identity)?.sequence <= self.sequence {
                return Ok(None);
            }
            operation(connection)
        })
    }
}

impl CommittedRecordSnapshot for Snapshot {
    fn reclamation_epoch(&self) -> Option<u64> {
        Some(self.reclamation_epoch)
    }
    fn provider_snapshot(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn commit_monitor(&self) -> Option<u64> {
        self.monitor
            .as_ref()
            .map(|monitor| monitor.load(std::sync::atomic::Ordering::Relaxed))
    }
    fn adopt_commit_monitor(&self, monitor: u64) -> bool {
        match &self.monitor {
            Some(current) => {
                current.store(monitor, std::sync::atomic::Ordering::Relaxed);
                true
            }
            None => false,
        }
    }
    fn sequence(&self) -> CommitSequence {
        self.sequence
    }

    fn visit_last_key(
        &self,
        prefix: &[u8],
        before: Option<&[u8]>,
        control: &StorageReadControl,
        visit: &mut RecordKeyVisitor<'_>,
    ) -> VersionResult<()> {
        last::visit(self, prefix, before, control, visit)
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

    fn visit_values(
        &self,
        keys: &mut RecordKeyIterator<'_>,
        control: &StorageReadControl,
        visit: &mut RecordPointVisitor<'_>,
    ) -> VersionResult<()> {
        control.check()?;
        // Read admission is lazy so an empty source does not acquire a physical transaction.
        let Some(first) = keys.next() else {
            return Ok(());
        };
        let first = first?;
        self.read(|connection| {
            let mut next = Some(first);
            while let Some(key) = next {
                control.check().map_err(VersionError::from)?;
                let mut more = true;
                value(connection, &key, self.sequence, control, &mut |record| {
                    more = visit(&key, record)?;
                    Ok(())
                })?;
                control.check().map_err(VersionError::from)?;
                if !more {
                    break;
                }
                drop(key);
                next = keys.next().transpose()?;
            }
            Ok(())
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
            values::visit(
                connection,
                prefix,
                after,
                limit,
                self.sequence,
                control,
                visit,
            )
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
            ordered::visit(
                connection,
                prefix,
                after,
                limit,
                self.sequence,
                control,
                &mut |key, info| {
                    Ok(visit(
                        key,
                        RecordMetadata {
                            revision: Some(CommitSequence::from_u64(info.revision)),
                            live: info.length.is_some(),
                        },
                    )?)
                },
            )
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
    let has_runs: bool = connection
        .prepare_cached("SELECT EXISTS(SELECT 1 FROM _uqa_mvcc_runs)")?
        .query_row([], |row| row.get(0))?;
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

#[derive(Clone, Copy)]
pub(super) struct Info {
    pub(super) revision: u64,
    pub(super) length: Option<usize>,
    run: bool,
}

pub(super) const POINT_METADATA_SQL: &str = concat!(
    "SELECT h.sequence, h.compacted, v.sequence, v.payload_length ",
    "FROM _uqa_mvcc_heads h LEFT JOIN _uqa_mvcc_version_metadata v ",
    "ON v.key = h.key AND v.sequence <= ?2 WHERE h.key = ?1 ",
    "ORDER BY v.sequence DESC LIMIT 1"
);

/// Version payloads of at most this many bytes are read with their metadata in one statement. The metadata length bounds `SQLite`'s copy before the payload is evaluated, and the payload is admitted against the read's memory budget before it is exposed; larger payloads keep the separate admitted read.
pub(super) const INLINE_PAYLOAD_BYTES: u16 = 16 * 1024;

/// [`POINT_METADATA_SQL`] with the selected version's payload when its length is at most `?3`. The metadata row is limited before the payload expression is evaluated, so at most one payload is copied.
pub(super) const POINT_VALUE_SQL: &str = concat!(
    "SELECT m.head, m.compacted, m.sequence, m.payload_length, ",
    "CASE WHEN m.payload_length <= ?3 THEN ",
    "(SELECT value FROM _uqa_mvcc_versions WHERE version_id = m.version_id AND key = ?1 AND sequence = m.sequence) END ",
    "FROM (SELECT h.sequence AS head, h.compacted AS compacted, v.sequence AS sequence, ",
    "v.payload_length AS payload_length, v.version_id AS version_id ",
    "FROM _uqa_mvcc_heads h LEFT JOIN _uqa_mvcc_version_metadata v ",
    "ON v.key = h.key AND v.sequence <= ?2 WHERE h.key = ?1 ",
    "ORDER BY v.sequence DESC LIMIT 1) m"
);

pub(super) fn info(
    connection: &Connection,
    key: &[u8],
    boundary: CommitSequence,
) -> PhysicalResult<Option<Info>> {
    // The descending version range supplies its first row directly. A scalar
    // predecessor subquery would seek the same version tree a second time.
    let mut statement = connection.prepare_cached(POINT_METADATA_SQL)?;
    let mut rows = statement.query(params![key, boundary.as_u64().to_be_bytes().as_slice()])?;
    let Some(row) = rows.next()? else {
        return run_info(connection, key, boundary);
    };
    point_info(row, boundary)
}

/// A key without a point head can only be stored in a compacted run.
fn run_info(
    connection: &Connection,
    key: &[u8],
    boundary: CommitSequence,
) -> PhysicalResult<Option<Info>> {
    Ok(runs::info(connection, key)?
        .filter(|(sequence, _)| *sequence <= boundary)
        .map(|(sequence, length)| Info {
            revision: sequence.as_u64(),
            length,
            run: true,
        }))
}

fn point_info(row: &rusqlite::Row<'_>, boundary: CommitSequence) -> PhysicalResult<Option<Info>> {
    checked_point_info(codec::decode_head(row)?, boundary, || {
        if matches!(row.get_ref(2)?, ValueRef::Null) {
            return Ok(None);
        }
        Ok(Some((
            codec::integer(codec::bytes(row, 2)?)?,
            row.get::<_, Option<i64>>(3)?,
        )))
    })
}

fn checked_point_info(
    (head, compacted): (CommitSequence, bool),
    boundary: CommitSequence,
    selected: impl FnOnce() -> PhysicalResult<Option<(u64, Option<i64>)>>,
) -> PhysicalResult<Option<Info>> {
    let head_visible = head <= boundary;
    if head_visible && compacted {
        return Ok(Some(Info {
            revision: head.as_u64(),
            length: None,
            run: false,
        }));
    }
    let Some((revision, length)) = selected()? else {
        if head_visible {
            return Err(VersionError::InvalidEncoding("record head has no version").into());
        }
        return Ok(None);
    };
    if head_visible && head.as_u64() != revision {
        return Err(VersionError::InvalidEncoding("record head has no matching version").into());
    }
    if revision == 0 {
        return Err(VersionError::InvalidEncoding("zero record revision").into());
    }
    Ok(Some(Info {
        revision,
        length: length.map(payload_length).transpose()?,
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
    let bindings = reserve_bindings(control, &[key])?;
    let mut statement = connection.prepare_cached(POINT_VALUE_SQL)?;
    let selected = {
        let mut rows = statement.query(params![
            key,
            boundary.as_u64().to_be_bytes().as_slice(),
            i64::from(INLINE_PAYLOAD_BYTES)
        ])?;
        match rows.next()? {
            Some(row) => {
                let info = point_info(row, boundary)?;
                if let Some(info) = info.filter(|info| {
                    info.length
                        .is_some_and(|length| length <= usize::from(INLINE_PAYLOAD_BYTES))
                }) {
                    return inline_value(row, info, max_bytes, control, visit);
                }
                Some(info)
            }
            None => None,
        }
    };
    drop(statement);
    let info = match selected {
        Some(info) => info,
        None => run_info(connection, key, boundary)?,
    };
    drop(bindings);
    let Some(info) = info else {
        control.cancellation().check().map_err(VersionError::from)?;
        visit(None)?;
        control.cancellation().check().map_err(VersionError::from)?;
        return Ok(());
    };
    value_from_info(connection, key, info, boundary, max_bytes, control, visit)
}

/// Visit a point payload that [`POINT_VALUE_SQL`] selected with its metadata. The admission order matches [`value_from_info`]: the declared length is checked and reserved before the payload's bytes are borrowed.
fn inline_value(
    row: &rusqlite::Row<'_>,
    info: Info,
    max_bytes: usize,
    control: &StorageReadControl,
    visit: &mut RecordValueVisitor<'_>,
) -> PhysicalResult<()> {
    let length = info.length.expect("inline payloads have a length");
    control
        .check_value_size(length, max_bytes)
        .map_err(VersionError::from)?;
    control.cancellation().check().map_err(VersionError::from)?;
    let _payload = control
        .memory()
        .reserve(length)
        .map_err(VersionError::from)?;
    let value = match row.get_ref(4)? {
        ValueRef::Blob(bytes) if bytes.len() == length => bytes,
        _ => {
            return Err(
                VersionError::InvalidEncoding("version size or type changed within a read").into(),
            )
        }
    };
    control.cancellation().check().map_err(VersionError::from)?;
    visit(Some(BorrowedRecord {
        revision: Some(CommitSequence::from_u64(info.revision)),
        value: Some(value),
    }))?;
    control.cancellation().check().map_err(VersionError::from)?;
    Ok(())
}

fn value_from_info(
    connection: &Connection,
    key: &[u8],
    info: Info,
    boundary: CommitSequence,
    max_bytes: usize,
    control: &StorageReadControl,
    visit: &mut RecordValueVisitor<'_>,
) -> PhysicalResult<()> {
    let _bindings = reserve_bindings(control, &[key])?;
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
        .prepare_cached("SELECT v.value FROM _uqa_mvcc_version_metadata m JOIN _uqa_mvcc_versions v ON v.version_id = m.version_id AND v.key = m.key AND v.sequence = m.sequence WHERE m.key = ?1 AND m.sequence = ?2")?;
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
        .prepare_cached(sizes)?
        .query_row(params![lower, upper], |row| row.get(0))
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
    let mut statement = connection.prepare_cached(data)?;
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
