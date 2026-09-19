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
mod reclamation;
mod retention;
#[cfg(test)]
mod tests;

use std::sync::Arc;

use redb::{
    Database, Durability, ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition,
    TableHandle, WriteTransaction,
};
use uqa_storage::mvcc::{
    resolve_prepared_receipt, CommitFailure, CommitReceipt, CommitResult, CommitSequence,
    CommitStatus, CommittedRecordSnapshot, DatabaseId, PreparedRecordCommit, StorageTransactionId,
    VersionError, VersionResult, VersionedPersistence,
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
/// Retained logical snapshots protect their predecessor histories; reclamation preserves head tombstones and all commit receipts. Logical Key/Value sessions use these records; Engine SQL isolation and shared-index publication require additional coordination.
#[derive(Clone)]
pub struct RedbRecordStore {
    database: Arc<Database>,
    identity: DatabaseId,
    snapshots: Arc<uqa_storage::mvcc::SnapshotRegistry>,
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
                if !matches!(format, 1..=32) {
                    return Err(VersionError::InvalidEncoding("unknown record format"));
                }
                if present != if format < 5 { 15 } else { 31 } {
                    return Err(VersionError::InvalidEncoding(
                        "identifier allocation table disagrees with record format",
                    ));
                }
                read_u64(&metadata, "allocated")?;
                read_u64(&metadata, "sequence")?;
                let identity = codec::database_id(&metadata)?;
                if format < 32 {
                    metadata
                        .insert("format", 32_u64.to_be_bytes().as_slice())
                        .map_err(redb_error)?;
                }
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
                let mut bytes = [0; 16];
                getrandom::fill(&mut bytes)
                    .map_err(|error| redb_error(std::io::Error::other(error.to_string())))?;
                metadata
                    .insert("database", bytes.as_slice())
                    .map_err(redb_error)?;
                metadata
                    .insert("format", 32_u64.to_be_bytes().as_slice())
                    .map_err(redb_error)?;
                metadata
                    .insert("allocated", 0_u64.to_be_bytes().as_slice())
                    .map_err(redb_error)?;
                metadata
                    .insert("sequence", 0_u64.to_be_bytes().as_slice())
                    .map_err(redb_error)?;
                DatabaseId::from_bytes(bytes)
            }
        };
        transaction.commit().map_err(redb_error)?;
        let snapshots = retention::registry(&database, identity)?;
        Ok(Self {
            database,
            identity,
            snapshots,
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
        receipts
            .insert(id.allocation(), receipt_bytes(receipt).as_slice())
            .map_err(redb_error)?;
        metadata
            .insert("sequence", sequence.as_u64().to_be_bytes().as_slice())
            .map_err(redb_error)?;
        control.cancellation().check()?;
        Ok((receipt, true))
    }
}

impl VersionedPersistence for RedbRecordStore {
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
        control.cancellation().check()?;
        let transaction = physical_writer(&self.database)?;
        let id = {
            let mut metadata = transaction.open_table(METADATA).map_err(redb_error)?;
            codec::validate_metadata(&metadata, self.identity)?;
            let allocation = read_u64(&metadata, "allocated")?
                .checked_add(1)
                .ok_or(VersionError::TransactionIdsExhausted)?;
            let id = StorageTransactionId::new(self.identity, allocation)?;
            transaction
                .open_table(TRANSACTIONS)
                .map_err(redb_error)?
                .insert(allocation, [0].as_slice())
                .map_err(redb_error)?;
            metadata
                .insert("allocated", allocation.to_be_bytes().as_slice())
                .map_err(redb_error)?;
            id
        };
        control.cancellation().check()?;
        transaction.commit().map_err(redb_error)?;
        Ok(id)
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
                    receipts
                        .insert(id.allocation(), [1].as_slice())
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
