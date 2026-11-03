//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Autonomous allocation history shares physical publication with the captured record boundary.

mod positions;

use std::{ops::Bound, sync::Arc};

use redb::{ReadableTable, TableDefinition, WriteTransaction};
use uqa_storage::{
    mvcc::{
        retain_backup_snapshot, retain_record_snapshot, CommitSequence, CommittedBackupSnapshot,
        CommittedRecordSnapshot, DatabaseId, IdentifierWatermarkVisitor, MetadataEpoch,
        SnapshotLease, VersionError, VersionResult, RECLAMATION_DOMAIN_PREFIX,
        RECLAMATION_EPOCH_NAMESPACE,
    },
    read_control::StorageReadControl,
};

use super::{codec, identifiers, physical_writer, read, redb_error, RedbRecordStore, METADATA};

pub(super) const BIRTHS: TableDefinition<&[u8], (u64, u64)> =
    TableDefinition::new("uqa_mvcc_identifier_epochs");
pub(super) const HISTORY: TableDefinition<(&[u8], u64), u64> =
    TableDefinition::new("uqa_mvcc_identifier_history");
pub(super) const EPOCH: &str = "backup_metadata_epoch";

/// Upgrade under the record owner's physical transaction; predecessor formats have no metadata history or retained backup cut.
pub(super) fn initialize(transaction: &WriteTransaction, predecessor: bool) -> VersionResult<()> {
    let mut births = transaction.open_table(BIRTHS).map_err(redb_error)?;
    let history = transaction.open_table(HISTORY).map_err(redb_error)?;
    let mut metadata = transaction.open_table(METADATA).map_err(redb_error)?;
    if predecessor {
        use redb::ReadableTableMetadata;
        if !births.is_empty().map_err(redb_error)? || !history.is_empty().map_err(redb_error)? {
            return Err(VersionError::InvalidEncoding(
                "predecessor contains backup metadata history",
            ));
        }
        let identifiers = transaction
            .open_table(identifiers::TABLE)
            .map_err(redb_error)?;
        for entry in identifiers.iter().map_err(redb_error)? {
            let (namespace, watermark) = entry.map_err(redb_error)?;
            let namespace = namespace.value();
            if !is_allocation(namespace) {
                continue;
            }
            if namespace.is_empty() {
                return Err(VersionError::InvalidEncoding("empty identifier namespace"));
            }
            codec::decode_u64(watermark.value())?;
            births.insert(namespace, (0, 0)).map_err(redb_error)?;
        }
        metadata
            .insert(EPOCH, 0_u64.to_be_bytes().as_slice())
            .map_err(redb_error)?;
    } else {
        codec::read_u64(&metadata, EPOCH)?;
    }
    Ok(())
}

fn is_allocation(namespace: &[u8]) -> bool {
    namespace != RECLAMATION_EPOCH_NAMESPACE && !namespace.starts_with(RECLAMATION_DOMAIN_PREFIX)
}

struct Snapshot {
    records: Arc<dyn CommittedRecordSnapshot>,
    database: Arc<redb::Database>,
    identity: DatabaseId,
    metadata: Arc<SnapshotLease<MetadataEpoch>>,
}

pub(super) fn capture(
    store: &RedbRecordStore,
    control: &StorageReadControl,
) -> VersionResult<Arc<dyn CommittedBackupSnapshot>> {
    control.cancellation().check()?;
    let mut sequence = CommitSequence::INITIAL;
    let mut reclamation_epoch = 0;
    let mut metadata_lease = None;
    let records = store.snapshots.capture(control, || {
        metadata_lease = Some(store.metadata_snapshots.capture(control, || {
            let transaction = physical_writer(&store.database)?;
            let epoch = {
                let mut metadata = transaction.open_table(METADATA).map_err(redb_error)?;
                codec::validate_metadata(&metadata, store.identity)?;
                sequence = CommitSequence::from_u64(codec::read_u64(&metadata, "sequence")?);
                reclamation_epoch = super::tombstones::epoch(
                    &transaction
                        .open_table(identifiers::TABLE)
                        .map_err(redb_error)?,
                )?;
                let epoch = MetadataEpoch::from_u64(codec::read_u64(&metadata, EPOCH)?);
                metadata
                    .insert(EPOCH, epoch.successor()?.as_u64().to_be_bytes().as_slice())
                    .map_err(redb_error)?;
                epoch
            };
            control.cancellation().check()?;
            transaction.commit().map_err(redb_error)?;
            Ok(epoch)
        })?);
        Ok(sequence)
    })?;
    let records = retain_record_snapshot(
        read::Snapshot {
            database: Arc::clone(&store.database),
            identity: store.identity,
            reclamation_epoch,
            lease: records,
        },
        control,
    )?;
    retain_backup_snapshot(
        Snapshot {
            records,
            database: Arc::clone(&store.database),
            identity: store.identity,
            metadata: metadata_lease.expect("successful joint capture retains metadata"),
        },
        control,
    )
}

/// Archive each namespace at most once between successive captures, in the same write transaction as its new watermark.
pub(super) fn retain_previous(
    transaction: &WriteTransaction,
    namespace: &[u8],
    previous: Option<u64>,
    epoch: u64,
) -> VersionResult<()> {
    let mut births = transaction.open_table(BIRTHS).map_err(redb_error)?;
    let head = births
        .get(namespace)
        .map_err(redb_error)?
        .map(|value| value.value());
    match (previous, head) {
        (Some(previous), Some((created, birth))) => {
            validate_epochs(created, birth, epoch)?;
            if epoch == birth {
                return Ok(());
            }
            let mut history = transaction.open_table(HISTORY).map_err(redb_error)?;
            if history
                .get((namespace, birth))
                .map_err(redb_error)?
                .is_some()
            {
                return Err(VersionError::InvalidEncoding(
                    "duplicate identifier metadata revision",
                ));
            }
            history
                .insert((namespace, birth), previous)
                .map_err(redb_error)?;
            births
                .insert(namespace, (created, epoch))
                .map_err(redb_error)?;
        }
        (None, None) => {
            births
                .insert(namespace, (epoch, epoch))
                .map_err(redb_error)?;
        }
        _ => {
            return Err(VersionError::InvalidEncoding(
                "identifier metadata head is missing or orphaned",
            ))
        }
    }
    Ok(())
}

fn validate_epochs(created: u64, birth: u64, current: u64) -> VersionResult<()> {
    if created > birth || birth > current {
        return Err(VersionError::InvalidEncoding(
            "invalid identifier metadata epochs",
        ));
    }
    Ok(())
}

impl CommittedBackupSnapshot for Snapshot {
    fn record_key_ceiling(
        &self,
        control: &StorageReadControl,
    ) -> VersionResult<Option<uqa_storage::mvcc::BackupCursorKey>> {
        self.record_ceiling(control)
    }
    fn identifier_namespace_ceiling(
        &self,
        control: &StorageReadControl,
    ) -> VersionResult<Option<uqa_storage::mvcc::BackupCursorKey>> {
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
        self.identity
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
        let (transaction, current_epoch) = self.read()?;
        let epoch = self.metadata_epoch().as_u64();
        let identifiers = transaction
            .open_table(identifiers::TABLE)
            .map_err(redb_error)?;
        let births = transaction.open_table(BIRTHS).map_err(redb_error)?;
        let history = transaction.open_table(HISTORY).map_err(redb_error)?;
        let start = after.map_or(Bound::Unbounded, Bound::Excluded);
        for entry in identifiers
            .range::<&[u8]>((start, Bound::Unbounded))
            .map_err(redb_error)?
            .take(limit)
        {
            control.cancellation().check()?;
            let (namespace, current) = entry.map_err(redb_error)?;
            let namespace = namespace.value();
            let watermark = if is_allocation(namespace) {
                let (created, birth) = births
                    .get(namespace)
                    .map_err(redb_error)?
                    .ok_or(VersionError::InvalidEncoding(
                        "missing identifier metadata head",
                    ))?
                    .value();
                validate_epochs(created, birth, current_epoch)?;
                if created > epoch {
                    None
                } else if birth <= epoch {
                    Some(codec::decode_u64(current.value())?)
                } else {
                    let value = history
                        .range((namespace, created)..=(namespace, epoch))
                        .map_err(redb_error)?
                        .next_back()
                        .transpose()
                        .map_err(redb_error)?
                        .map(|(_, value)| value.value())
                        .ok_or(VersionError::InvalidEncoding(
                            "missing retained identifier watermark",
                        ))?;
                    if value > codec::decode_u64(current.value())? {
                        return Err(VersionError::InvalidEncoding(
                            "identifier history exceeds its current watermark",
                        ));
                    }
                    Some(value)
                }
            } else {
                None
            };
            if !visit(namespace, watermark)? {
                break;
            }
        }
        control.cancellation().check()?;
        Ok(())
    }
}

/// Keep the predecessor visible to the oldest metadata lease and all later revisions. The physical writer is already admitted by both retention registries.
pub(super) fn reclaim(
    transaction: &WriteTransaction,
    oldest: Option<MetadataEpoch>,
    control: &StorageReadControl,
) -> VersionResult<()> {
    let epoch = codec::read_u64(
        &transaction.open_table(METADATA).map_err(redb_error)?,
        EPOCH,
    )?;
    let horizon = oldest.map_or(epoch, MetadataEpoch::as_u64);
    if horizon > epoch {
        return Err(VersionError::InvalidEncoding(
            "metadata lease exceeds the current epoch",
        ));
    }
    let births = transaction.open_table(BIRTHS).map_err(redb_error)?;
    let mut history = transaction.open_table(HISTORY).map_err(redb_error)?;
    for entry in births.iter().map_err(redb_error)? {
        control.cancellation().check()?;
        let (namespace, head) = entry.map_err(redb_error)?;
        let namespace = namespace.value();
        let (created, birth) = head.value();
        validate_epochs(created, birth, epoch)?;
        let cutoff = if birth <= horizon {
            Some(birth)
        } else {
            history
                .range((namespace, 0)..=(namespace, horizon))
                .map_err(redb_error)?
                .next_back()
                .transpose()
                .map_err(redb_error)?
                .map(|(key, _)| key.value().1)
        };
        if let Some(cutoff) = cutoff.filter(|cutoff| *cutoff > 0) {
            loop {
                control.cancellation().check()?;
                let obsolete = history
                    .range((namespace, 0)..(namespace, cutoff))
                    .map_err(redb_error)?
                    .next()
                    .transpose()
                    .map_err(redb_error)?
                    .map(|(key, _)| key.value().1);
                let Some(obsolete) = obsolete else {
                    break;
                };
                history.remove((namespace, obsolete)).map_err(redb_error)?;
            }
        }
    }
    Ok(())
}
