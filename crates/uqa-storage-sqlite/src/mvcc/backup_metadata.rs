//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One durable cut retains logical records and autonomous identifier generations without a long physical read.

mod history;
mod positions;
pub(super) use history::{initialize, reclaim, retain_previous, TABLES};

use super::{admission, codec, native, read, tombstones, SQLiteRecordStore};

pub(super) fn validate(connection: &rusqlite::Connection) -> super::PhysicalResult<()> {
    history::epoch(connection)?;
    Ok(())
}
use rusqlite::params;
use std::sync::Arc;
use uqa_storage::{
    mvcc::{
        retain_backup_snapshot, retain_record_snapshot, CommitSequence, CommittedBackupSnapshot,
        CommittedRecordSnapshot, DatabaseId, IdentifierWatermarkVisitor, MetadataEpoch,
        SnapshotLease, VersionError, VersionResult, VersionedPersistence,
    },
    read_control::StorageReadControl,
};

struct Snapshot {
    store: SQLiteRecordStore,
    records: Arc<dyn CommittedRecordSnapshot>,
    metadata: Arc<SnapshotLease<MetadataEpoch>>,
}

pub(super) fn capture(
    store: &SQLiteRecordStore,
    control: &StorageReadControl,
) -> VersionResult<Arc<dyn CommittedBackupSnapshot>> {
    control.cancellation().check()?;
    let monitor = store.commit_monitor_version()?;
    let mut sequence = CommitSequence::INITIAL;
    let mut reclamation_epoch = 0;
    let mut metadata = None;
    let records = store.snapshots.capture(control, || {
        metadata = Some(store.metadata_snapshots.capture(control, || {
            store.with_write(control, |connection| {
                let _permit = admission::permit(connection, control)?;
                let transaction = admission::begin(connection, control)?;
                native::check_mapping(&transaction, store.native)?;
                sequence = codec::header(&transaction, store.identity)?.sequence;
                reclamation_epoch = tombstones::epoch(&transaction)?;
                let epoch = MetadataEpoch::from_u64(history::epoch(&transaction)?);
                transaction.execute(
                    "UPDATE _uqa_mvcc_backup_metadata SET epoch = ?1 WHERE singleton = 1",
                    [epoch.successor()?.as_u64().to_be_bytes().as_slice()],
                )?;
                transaction.execute(
                    "UPDATE _uqa_mvcc_metadata SET sequence_durable = sequence WHERE singleton = 1",
                    [],
                )?;
                admission::commit(transaction, control)?;
                Ok(epoch)
            })
        })?);
        Ok(sequence)
    })?;
    let records = retain_record_snapshot(
        read::Snapshot {
            store: store.clone(),
            sequence,
            reclamation_epoch,
            monitor: monitor.map(std::sync::atomic::AtomicU64::new),
            table_owners: read::table_owners::TableOwners::default(),
            row_presence: read::row_presence::RowPresence::default(),
            _lease: records,
        },
        control,
    )?;
    retain_backup_snapshot(
        Snapshot {
            store: store.clone(),
            records,
            metadata: metadata.expect("successful joint capture retains metadata"),
        },
        control,
    )
}

impl CommittedBackupSnapshot for Snapshot {
    fn record_key_ceiling(
        &self,
        control: &StorageReadControl,
    ) -> VersionResult<Option<uqa_core::memory::BudgetedVec<u8>>> {
        control.cancellation().check()?;
        let result = self.read(|connection, _| read::key_ceiling(connection, control))?;
        control.cancellation().check()?;
        Ok(result)
    }
    fn identifier_namespace_ceiling(
        &self,
        control: &StorageReadControl,
    ) -> VersionResult<Option<uqa_core::memory::BudgetedVec<u8>>> {
        self.identifier_ceiling(control)
    }
    fn visit_record_positions(
        &self,
        after: Option<&[u8]>,
        limit: usize,
        control: &StorageReadControl,
        visit: &mut uqa_storage::mvcc::BackupRecordPositionVisitor<'_>,
    ) -> VersionResult<()> {
        self.record_positions(after, limit, control, visit)
    }
    fn records(&self) -> &dyn CommittedRecordSnapshot {
        self.records.as_ref()
    }
    fn database_id(&self) -> DatabaseId {
        self.store.identity
    }
    fn metadata_epoch(&self) -> MetadataEpoch {
        self.metadata.sequence()
    }

    fn visit_identifier_watermarks(
        &self,
        after: Option<&[u8]>,
        limit: usize,
        control: &StorageReadControl,
        visit: &mut IdentifierWatermarkVisitor<'_>,
    ) -> VersionResult<()> {
        control.cancellation().check()?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        self.read(|connection, current| {
            let epoch = self.metadata_epoch().as_u64();
            let _after = crate::read_control::reserve_bindings(control, &[after.unwrap_or_default()])?;
            let mut statement = connection.prepare_cached("SELECT i.namespace, i.watermark, e.created, e.head FROM _uqa_mvcc_identifiers AS i LEFT JOIN _uqa_mvcc_identifier_epochs AS e ON e.namespace = i.namespace WHERE i.namespace > ?1 ORDER BY i.namespace LIMIT ?2")?;
            let mut rows = statement.query(params![after.unwrap_or_default(), limit])?;
            while let Some(row) = rows.next()? {
                control.cancellation().check().map_err(VersionError::from)?;
                let namespace = codec::bytes(row, 0)?;
                let value = if history::is_allocation(namespace) {
                    let watermark = codec::integer(codec::bytes(row, 1)?)?;
                    let created = codec::integer(codec::bytes(row, 2)?)?;
                    let head = codec::integer(codec::bytes(row, 3)?)?;
                    history::validate_epochs(created, head, current)?;
                    if created > epoch { None } else if head <= epoch { Some(watermark) } else {
                        let _bindings = crate::read_control::reserve_bindings(control, &[namespace])?;
                        let value = history::previous(connection, namespace, created, epoch)?.1;
                        if value > watermark {
                            return Err(VersionError::InvalidEncoding("identifier history exceeds its current watermark").into());
                        }
                        Some(value)
                    }
                } else { None };
                if !visit(namespace, value)? { break; }
            }
            control.cancellation().check().map_err(VersionError::from)?;
            Ok(())
        })
    }
}
