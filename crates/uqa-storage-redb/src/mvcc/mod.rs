//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Short redb transactions persist versioned records and authoritative commit receipts.

mod codec;
mod identifiers;
mod migration;
mod read;
mod receipts;
mod reclamation;
pub(crate) mod restore;
mod retention;
mod serializable;
#[cfg(test)]
mod tests;

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

const METADATA: TableDefinition<&str, &[u8]> = TableDefinition::new("uqa_mvcc_metadata");
const HEADS: TableDefinition<&[u8], u64> = TableDefinition::new("uqa_mvcc_heads");
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
        let identity = {
            let mut metadata = transaction.open_table(METADATA).map_err(redb_error)?;
            let heads = transaction.open_table(HEADS).map_err(redb_error)?;
            let versions = transaction.open_table(VERSIONS).map_err(redb_error)?;
            let receipts = transaction.open_table(TRANSACTIONS).map_err(redb_error)?;
            let identifiers = transaction
                .open_table(identifiers::TABLE)
                .map_err(redb_error)?;
            let initialized = metadata
                .get("format")
                .map_err(redb_error)?
                .map(|value| codec::decode_u64(value.value()))
                .transpose()?;
            if let Some(format) = initialized {
                if !matches!(format, 1..=45) {
                    return Err(VersionError::InvalidEncoding("unknown record format"));
                }
                if present != if format < 5 { 15 } else { 31 } {
                    return Err(VersionError::InvalidEncoding(
                        "identifier allocation table disagrees with record format",
                    ));
                }
                let allocated = read_u64(&metadata, "allocated")?;
                read_u64(&metadata, "sequence")?;
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
                if format < 45 {
                    metadata
                        .insert("format", 45_u64.to_be_bytes().as_slice())
                        .map_err(redb_error)?;
                }
                codec::receipt_limit(&metadata)?;
                identity
            } else {
                if metadata
                    .iter()
                    .map_err(redb_error)?
                    .next()
                    .transpose()
                    .map_err(redb_error)?
                    .is_some()
                    || heads
                        .iter()
                        .map_err(redb_error)?
                        .next()
                        .transpose()
                        .map_err(redb_error)?
                        .is_some()
                    || versions
                        .iter()
                        .map_err(redb_error)?
                        .next()
                        .transpose()
                        .map_err(redb_error)?
                        .is_some()
                    || receipts
                        .iter()
                        .map_err(redb_error)?
                        .next()
                        .transpose()
                        .map_err(redb_error)?
                        .is_some()
                    || !identifiers.is_empty().map_err(redb_error)?
                {
                    return Err(VersionError::InvalidEncoding(
                        "uninitialized metadata has record data",
                    ));
                }
                initialize_record_metadata(&mut metadata)?
            }
        };
        transaction.commit().map_err(redb_error)?;
        let snapshots = retention::registry(&database, identity)?;
        let serializable = serializable::registry(&database, identity, false)?;
        let receipts = serializable::registry(&database, identity, true)?;
        Ok(Self {
            database,
            identity,
            snapshots,
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
        prepared.validate(control.cancellation(), |key| {
            Ok(heads
                .get(key)
                .map_err(redb_error)?
                .map(|version| CommitSequence::from_u64(version.value())))
        })?;
        let sequence = if prepared.records().is_empty() {
            current
        } else {
            current.successor()?
        };
        let mut maximum = 0;
        for write in prepared.records() {
            control.cancellation().check()?;
            maximum = maximum.max(
                write
                    .value()
                    .map_or(0, <[u8]>::len)
                    .checked_add(1)
                    .ok_or(VersionError::InvalidEncoding("record size overflow"))?,
            );
        }
        let mut workspace = control.memory().reserve(maximum)?;
        let mut encoded = Vec::new();
        encoded
            .try_reserve_exact(maximum)
            .map_err(|error| uqa_storage::StorageBackendError::Memory(error.into()))?;
        workspace.grow(encoded.capacity() - maximum)?;
        let mut versions = transaction.open_table(VERSIONS).map_err(redb_error)?;
        for write in prepared.records() {
            control.cancellation().check()?;
            let value = write.value();
            // redb's insert_reserve allocates a temporary value for every row. Reuse one charged encoding buffer for this bounded physical commit instead.
            encoded.clear();
            encoded.push(u8::from(value.is_some()));
            if let Some(value) = value {
                for source in value.chunks(65536) {
                    control.cancellation().check()?;
                    encoded.extend_from_slice(source);
                }
            }
            versions
                .insert((write.key(), sequence.as_u64()), encoded.as_slice())
                .map_err(redb_error)?;
            heads
                .insert(write.key(), sequence.as_u64())
                .map_err(redb_error)?;
        }
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
        let lease = self.snapshots.capture(control, || {
            let transaction = self.database.begin_read().map_err(redb_error)?;
            let metadata = transaction.open_table(METADATA).map_err(redb_error)?;
            codec::validate_metadata(&metadata, self.identity)?;
            Ok(CommitSequence::from_u64(read_u64(&metadata, "sequence")?))
        })?;
        uqa_storage::mvcc::retain_record_snapshot(
            read::Snapshot {
                database: Arc::clone(&self.database),
                identity: self.identity,
                sequence: lease.sequence(),
                _lease: lease,
            },
            control,
        )
    }

    fn reclaim_versions(&self, control: &StorageReadControl) -> VersionResult<u64> {
        self.snapshots.reclaim(control, |oldest| {
            reclamation::reclaim(self, oldest, control)
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
        .insert("format", 45_u64.to_be_bytes().as_slice())
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

fn record_table_presence(transaction: &WriteTransaction) -> VersionResult<u8> {
    let names = [
        METADATA.name(),
        HEADS.name(),
        VERSIONS.name(),
        TRANSACTIONS.name(),
        identifiers::TABLE.name(),
    ];
    let mut present = 0_u8;
    for table in transaction.list_tables().map_err(redb_error)? {
        if let Some(position) = names.iter().position(|name| *name == table.name()) {
            present |= 1 << position;
        }
    }
    if present != 0 && present != 15 && present != 31 {
        return Err(VersionError::InvalidEncoding("incomplete record table set"));
    }
    Ok(present)
}
