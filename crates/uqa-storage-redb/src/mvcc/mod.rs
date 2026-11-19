//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Short redb transactions persist versioned records and authoritative commit receipts.

#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
pub(crate) mod backup_import;
mod backup_metadata;
mod codec;
mod heads;
mod identifiers;
mod image_fingerprints;
mod large_values;
mod migration;
mod read;
mod receipts;
mod reclamation;
mod resources;
pub(crate) mod restore;
mod retention;
mod serializable;
#[cfg(test)]
mod tests;
mod tombstones;

use std::sync::Arc;

use redb::{
    Database, Durability, ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition,
    TableHandle, WriteTransaction,
};
use uqa_storage::mvcc::{
    resolve_prepared_receipt, CommitFailure, CommitReceipt, CommitResult, CommitSequence,
    CommitStatus, CommittedRecordSnapshot, DatabaseId, PreparedRecordCommit,
    ReceiptAcknowledgement, RetainedTransactionAllocation, StorageTransactionId, VersionError,
    VersionResult, VersionedPersistence, DEFAULT_RECEIPT_RETENTION_LIMIT,
};
use uqa_storage::read_control::StorageReadControl;

use crate::error::redb_error;
use codec::{read_u64, receipt_bytes, status};

/// New writers atomically publish record image digests alongside each immutable revision.
const RECORD_FORMAT: u64 = 67;

const METADATA: TableDefinition<&str, &[u8]> = TableDefinition::new("uqa_mvcc_metadata");
const HEADS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("uqa_mvcc_image_heads");
const VERSIONS: TableDefinition<(&[u8], u64), &[u8]> = TableDefinition::new("uqa_mvcc_versions");
const TRANSACTIONS: TableDefinition<u64, &[u8]> = TableDefinition::new("uqa_mvcc_transactions");

/// Physical record persistence over one shared redb file owner. Reads retain logical sequence boundaries, and native writers exist only inside allocation, commit and abort calls.
///
/// Retained logical snapshots protect their predecessor histories; receipt reclamation separately preserves explicit resolution owners and every durable SSI publication. Logical Key/Value sessions use these records; Engine SQL isolation and shared-index publication require additional coordination.
#[derive(Clone)]
pub struct RedbRecordStore {
    database: Arc<Database>,
    identity: DatabaseId,
    snapshots: Arc<uqa_storage::mvcc::SnapshotRegistry>,
    metadata_snapshots: Arc<uqa_storage::mvcc::SnapshotRegistry<uqa_storage::mvcc::MetadataEpoch>>,
    serializable: Arc<uqa_storage::mvcc::LocalSerializableState>,
    receipts: Arc<uqa_storage::mvcc::LocalSerializableState>,
}

impl RedbRecordStore {
    pub(crate) fn migrate_key_value(&self) -> VersionResult<()> {
        migration::migrate(&self.database, self.identity)
    }

    pub(crate) fn new(database: Arc<Database>) -> VersionResult<Self> {
        let transaction = physical_writer(&database)?;
        let present = record_table_presence(&transaction)?;
        let initialize_backup_metadata;
        let identity = {
            let mut metadata = transaction.open_table(METADATA).map_err(redb_error)?;
            let versions = transaction.open_table(VERSIONS).map_err(redb_error)?;
            let receipts = transaction.open_table(TRANSACTIONS).map_err(redb_error)?;
            let mut identifiers = transaction
                .open_table(identifiers::TABLE)
                .map_err(redb_error)?;
            let initialized = metadata
                .get("format")
                .map_err(redb_error)?
                .map(|value| codec::decode_u64(value.value()))
                .transpose()?;
            if let Some(format) = initialized {
                let expected = match format {
                    1..=4 => 15,
                    5..=63 => 31,
                    64 => 127,
                    65 => 255,
                    66 => 509,
                    RECORD_FORMAT => 2045,
                    _ => return Err(VersionError::InvalidEncoding("unknown record format")),
                };
                if present != expected {
                    return Err(VersionError::InvalidEncoding(
                        "record tables disagree with record format",
                    ));
                }
                if format < 66 {
                    heads::migrate_heads(&transaction, format)?;
                }
                let allocated = read_u64(&metadata, "allocated")?;
                image_fingerprints::initialize_boundary(&mut metadata, format)?;
                let identity = codec::database_id(&metadata)?;
                if format < 43 {
                    // Predecessor receipts retain manual resolution ownership. Validate their old encoding before exposing the new acknowledgement tags.
                    for entry in receipts.iter().map_err(redb_error)? {
                        let (allocation, receipt) = entry.map_err(redb_error)?;
                        if allocation.value() == 0
                            || allocation.value() > allocated
                            || codec::receipt_tag(receipt.value())? > 2
                        {
                            return Err(VersionError::InvalidEncoding("invalid legacy receipt"));
                        }
                    }
                    metadata
                        .insert(
                            "receipt_limit",
                            DEFAULT_RECEIPT_RETENTION_LIMIT.to_be_bytes().as_slice(),
                        )
                        .map_err(redb_error)?;
                }
                if format < 53 {
                    identifiers::consolidate_diskann_generations(&mut identifiers)?;
                }
                initialize_backup_metadata = format < 64;
                if format < RECORD_FORMAT {
                    metadata
                        .insert("format", RECORD_FORMAT.to_be_bytes().as_slice())
                        .map_err(redb_error)?;
                }
                codec::receipt_limit(&metadata)?;
                identity
            } else {
                if !metadata.is_empty().map_err(redb_error)?
                    || !versions.is_empty().map_err(redb_error)?
                    || !receipts.is_empty().map_err(redb_error)?
                    || !identifiers.is_empty().map_err(redb_error)?
                {
                    return Err(VersionError::InvalidEncoding(
                        "uninitialized metadata has record data",
                    ));
                }
                heads::initialize_empty_heads(&transaction, present & 2 != 0)?;
                large_values::Writer::open(&transaction)?.require_empty()?;
                initialize_backup_metadata = true;
                initialize_record_metadata(&mut metadata)?
            }
        };
        transaction
            .open_table(image_fingerprints::TABLE)
            .map_err(redb_error)?;
        drop(large_values::Writer::open(&transaction)?);
        backup_metadata::initialize(&transaction, initialize_backup_metadata)?;
        transaction.commit().map_err(redb_error)?;
        let snapshots = retention::registry(&database, identity)?;
        let metadata_snapshots = retention::metadata_registry(&database, identity)?;
        let serializable = serializable::registry(&database, identity, false)?;
        let receipts = serializable::registry(&database, identity, true)?;
        Ok(Self {
            database,
            identity,
            snapshots,
            metadata_snapshots,
            serializable,
            receipts,
        })
    }

    fn check_identity(&self, transaction: StorageTransactionId) -> VersionResult<()> {
        if transaction.database() != self.identity {
            return Err(VersionError::WrongDatabase);
        }
        Ok(())
    }

    fn prepare_commit(
        transaction: &WriteTransaction,
        id: StorageTransactionId,
        prepared: &PreparedRecordCommit,
        control: &StorageReadControl,
    ) -> VersionResult<(CommitReceipt, bool)> {
        let mut metadata = transaction.open_table(METADATA).map_err(redb_error)?;
        codec::validate_metadata(&metadata, id.database())?;
        let mut receipts = transaction.open_table(TRANSACTIONS).map_err(redb_error)?;
        if let Some(receipt) =
            resolve_prepared_receipt(status(&receipts, id)?, id, prepared.fingerprint())?
        {
            return Ok((receipt, false));
        }
        let mut heads = transaction.open_table(HEADS).map_err(redb_error)?;
        let current = CommitSequence::from_u64(read_u64(&metadata, "sequence")?);
        prepared.validate_snapshot(current)?;
        tombstones::validate(
            &transaction
                .open_table(identifiers::TABLE)
                .map_err(redb_error)?,
            prepared,
            control,
        )?;
        prepared.validate(control, |key| {
            heads
                .get(key)
                .map_err(redb_error)?
                .map(|version| {
                    Ok(CommitSequence::from_u64(
                        heads::RecordHead::decode(version.value())?.revision,
                    ))
                })
                .transpose()
        })?;
        let sequence = if prepared.is_empty() {
            current
        } else {
            current.successor()?
        };
        heads::write_batch(transaction, &mut heads, sequence, prepared, control)?;
        let receipt = CommitReceipt {
            transaction: id,
            sequence,
            fingerprint: prepared.fingerprint(),
        };
        let mut encoded_receipt = receipt_bytes(receipt);
        encoded_receipt[0] |= codec::receipt_owner(&receipts, id)?;
        receipts
            .insert(id.allocation(), encoded_receipt.as_slice())
            .map_err(redb_error)?;
        metadata
            .insert("sequence", sequence.as_u64().to_be_bytes().as_slice())
            .map_err(redb_error)?;
        control.cancellation().check()?;
        Ok((receipt, true))
    }
}

impl VersionedPersistence for RedbRecordStore {
    fn diskann_population_record_layout(
        &self,
    ) -> Option<&dyn uqa_storage::mvcc::DiskANNPopulationRecordLayout> {
        Some(&uqa_storage::key_value::KeyValueDiskANNPopulationRecords)
    }

    fn resource_leases(&self) -> Option<&dyn uqa_storage::mvcc::ResourceLeaseProvider> {
        Some(self)
    }

    fn serializable_coordinator(&self) -> Option<&dyn uqa_storage::mvcc::SerializableCoordinator> {
        Some(self)
    }

    fn identifier_watermark(
        &self,
        namespace: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Option<u64>> {
        identifiers::read(self, namespace, control)
    }

    fn allocate_identifiers(
        &self,
        namespace: &[u8],
        request: uqa_storage::mvcc::IdentifierRequest,
        control: &StorageReadControl,
    ) -> VersionResult<uqa_storage::mvcc::IdentifierAllocation> {
        identifiers::allocate(self, namespace, request, control)
    }

    fn database_id(&self) -> DatabaseId {
        self.identity
    }

    fn graph_record_layout(&self) -> Option<&dyn uqa_storage::mvcc::GraphRecordLayout> {
        Some(&uqa_storage::key_value::KeyValueGraphRecords)
    }

    fn allocate_transaction(
        &self,
        control: &StorageReadControl,
    ) -> VersionResult<StorageTransactionId> {
        receipts::allocate(self, false, control, |_| Ok(()))
    }

    fn allocate_managed_transaction(
        &self,
        control: &StorageReadControl,
    ) -> VersionResult<RetainedTransactionAllocation> {
        receipts::allocate_managed(self, control)
    }

    fn acknowledge_transaction(
        &self,
        acknowledgement: ReceiptAcknowledgement,
        control: &StorageReadControl,
    ) -> VersionResult<()> {
        receipts::acknowledge(self, acknowledgement, control)
    }

    fn reclaim_transaction_receipts(&self, control: &StorageReadControl) -> VersionResult<u64> {
        receipts::reclaim(self, control)
    }

    fn snapshot(
        &self,
        control: &StorageReadControl,
    ) -> VersionResult<Arc<dyn CommittedRecordSnapshot>> {
        control.cancellation().check()?;
        let mut reclamation_epoch = 0;
        let lease = self.snapshots.capture(control, || {
            let transaction = self.database.begin_read().map_err(redb_error)?;
            let metadata = transaction.open_table(METADATA).map_err(redb_error)?;
            codec::validate_metadata(&metadata, self.identity)?;
            reclamation_epoch = tombstones::epoch(
                &transaction
                    .open_table(identifiers::TABLE)
                    .map_err(redb_error)?,
            )?;
            Ok(CommitSequence::from_u64(read_u64(&metadata, "sequence")?))
        })?;
        uqa_storage::mvcc::retain_record_snapshot(
            read::Snapshot {
                database: Arc::clone(&self.database),
                identity: self.identity,
                reclamation_epoch,
                lease,
            },
            control,
        )
    }

    fn reclaim_versions(&self, control: &StorageReadControl) -> VersionResult<u64> {
        self.snapshots.reclaim(control, |oldest| {
            self.metadata_snapshots.reclaim(control, |oldest_metadata| {
                reclamation::reclaim(self, oldest, oldest_metadata, control)
            })
        })
    }

    fn capture_backup_snapshot(
        &self,
        control: &StorageReadControl,
    ) -> VersionResult<Option<Arc<dyn uqa_storage::mvcc::CommittedBackupSnapshot>>> {
        backup_metadata::capture(self, control).map(Some)
    }

    fn reclaim_tombstones(
        &self,
        request: &uqa_storage::mvcc::TombstoneReclamationRequest<'_>,
        control: &StorageReadControl,
    ) -> VersionResult<uqa_storage::mvcc::TombstoneReclamationStep> {
        request.validate(control)?;
        self.snapshots.reclaim(control, |oldest| {
            if oldest.is_some() {
                return Ok(uqa_storage::mvcc::TombstoneReclamationStep::Retained);
            }
            tombstones::reclaim(self, request, control)
        })
    }

    fn commit(
        &self,
        id: StorageTransactionId,
        prepared: &PreparedRecordCommit,
        control: &StorageReadControl,
    ) -> CommitResult {
        control.cancellation().check().map_err(VersionError::from)?;
        self.check_identity(id)?;
        let transaction = physical_writer(&self.database)?;
        let (receipt, changed) = Self::prepare_commit(&transaction, id, prepared, control)?;
        if !changed {
            return Ok(receipt);
        }
        if let Err(error) = transaction.commit() {
            return Err(CommitFailure::Indeterminate {
                transaction: id,
                source: redb_error(error),
            });
        }
        Ok(receipt)
    }

    fn commit_status(
        &self,
        id: StorageTransactionId,
        control: &StorageReadControl,
    ) -> VersionResult<CommitStatus> {
        control.cancellation().check()?;
        self.check_identity(id)?;
        let transaction = self.database.begin_read().map_err(redb_error)?;
        codec::validate_metadata(
            &transaction.open_table(METADATA).map_err(redb_error)?,
            self.identity,
        )?;
        status(
            &transaction.open_table(TRANSACTIONS).map_err(redb_error)?,
            id,
        )
    }

    fn abort(
        &self,
        id: StorageTransactionId,
        control: &StorageReadControl,
    ) -> VersionResult<CommitStatus> {
        control.cancellation().check()?;
        self.check_identity(id)?;
        let transaction = physical_writer(&self.database)?;
        codec::validate_metadata(
            &transaction.open_table(METADATA).map_err(redb_error)?,
            self.identity,
        )?;
        let outcome = {
            let mut receipts = transaction.open_table(TRANSACTIONS).map_err(redb_error)?;
            match status(&receipts, id)? {
                CommitStatus::Pending => {
                    let owner = codec::receipt_owner(&receipts, id)?;
                    receipts
                        .insert(id.allocation(), [1 | owner].as_slice())
                        .map_err(redb_error)?;
                    CommitStatus::Aborted
                }
                outcome => return Ok(outcome),
            }
        };
        control.cancellation().check()?;
        transaction.commit().map_err(redb_error)?;
        Ok(outcome)
    }
}

fn physical_writer(database: &Database) -> VersionResult<WriteTransaction> {
    let mut transaction = database.begin_write().map_err(redb_error)?;
    transaction
        .set_durability(Durability::Immediate)
        .map_err(redb_error)?;
    Ok(transaction)
}

fn initialize_record_metadata(
    metadata: &mut redb::Table<'_, &'static str, &'static [u8]>,
) -> VersionResult<DatabaseId> {
    let mut bytes = [0; 16];
    getrandom::fill(&mut bytes)
        .map_err(|error| redb_error(std::io::Error::other(error.to_string())))?;
    metadata
        .insert("database", bytes.as_slice())
        .map_err(redb_error)?;
    metadata
        .insert("format", RECORD_FORMAT.to_be_bytes().as_slice())
        .map_err(redb_error)?;
    metadata
        .insert(
            image_fingerprints::LEGACY_THROUGH,
            0_u64.to_be_bytes().as_slice(),
        )
        .map_err(redb_error)?;
    metadata
        .insert("allocated", 0_u64.to_be_bytes().as_slice())
        .map_err(redb_error)?;
    metadata
        .insert("sequence", 0_u64.to_be_bytes().as_slice())
        .map_err(redb_error)?;
    metadata
        .insert(
            "receipt_limit",
            DEFAULT_RECEIPT_RETENTION_LIMIT.to_be_bytes().as_slice(),
        )
        .map_err(redb_error)?;
    Ok(DatabaseId::from_bytes(bytes))
}

fn record_table_presence(transaction: &WriteTransaction) -> VersionResult<u16> {
    let names = [
        METADATA.name(),
        heads::LEGACY_HEADS.name(),
        VERSIONS.name(),
        TRANSACTIONS.name(),
        identifiers::TABLE.name(),
        backup_metadata::BIRTHS.name(),
        backup_metadata::HISTORY.name(),
        image_fingerprints::TABLE.name(),
        HEADS.name(),
        large_values::BODIES.name(),
        large_values::OWNERS.name(),
    ];
    let mut present = 0_u16;
    for table in transaction.list_tables().map_err(redb_error)? {
        if let Some(position) = names.iter().position(|name| *name == table.name()) {
            present |= 1 << position;
        }
    }
    if !matches!(present, 0 | 15 | 31 | 127 | 255 | 509 | 2045) {
        return Err(VersionError::InvalidEncoding("incomplete record table set"));
    }
    Ok(present)
}
