//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded receipt admission and explicit resolution ownership share short durable writes.

use redb::{ReadableTable, ReadableTableMetadata};
use uqa_storage::{
    mvcc::{
        receipt_lease_id, CommitStatus, ReceiptAcknowledgement, RetainedTransactionAllocation,
        SerializableCoordinator, SerializableGraph, SerializableLeases, StorageTransactionId,
        VersionError, VersionResult,
    },
    read_control::StorageReadControl,
};

use super::{codec, physical_writer, RedbRecordStore, METADATA, TRANSACTIONS};
use crate::error::redb_error;

impl RedbRecordStore {
    /// Set the database-wide maximum retained receipt count. Pending, unacknowledged and SSI-referenced receipts all count. Lowering the positive limit preserves existing entries and prevents allocation until reclamation makes room. The default is 65,536 entries.
    pub fn set_receipt_retention_limit(
        &self,
        limit: u64,
        control: &StorageReadControl,
    ) -> VersionResult<()> {
        control.cancellation().check()?;
        if limit == 0 {
            return Err(VersionError::InvalidEncoding(
                "invalid receipt retention limit",
            ));
        }
        let transaction = physical_writer(&self.database)?;
        {
            let mut metadata = transaction.open_table(METADATA).map_err(redb_error)?;
            codec::validate_metadata(&metadata, self.identity)?;
            metadata
                .insert("receipt_limit", limit.to_be_bytes().as_slice())
                .map_err(redb_error)?;
        }
        control.cancellation().check()?;
        transaction.commit().map_err(redb_error)?;
        Ok(())
    }
}

pub(super) fn allocate(
    store: &RedbRecordStore,
    managed: bool,
    control: &StorageReadControl,
    retain: impl FnOnce(StorageTransactionId) -> VersionResult<()>,
) -> VersionResult<StorageTransactionId> {
    control.cancellation().check()?;
    let transaction = physical_writer(&store.database)?;
    let id = {
        let mut metadata = transaction.open_table(METADATA).map_err(redb_error)?;
        codec::validate_metadata(&metadata, store.identity)?;
        let allocation = codec::read_u64(&metadata, "allocated")?
            .checked_add(1)
            .ok_or(VersionError::TransactionIdsExhausted)?;
        let mut receipts = transaction.open_table(TRANSACTIONS).map_err(redb_error)?;
        let limit = codec::receipt_limit(&metadata)?;
        if receipts.len().map_err(redb_error)? >= limit {
            return Err(VersionError::ReceiptRetentionExhausted { limit });
        }
        let id = StorageTransactionId::new(store.identity, allocation)?;
        // The managed lease is published under receipt admission before Pending can become durable. Allocation failure releases it without publishing a partial receipt.
        retain(id)?;
        receipts
            .insert(
                allocation,
                [if managed { codec::MANAGED_RECEIPT } else { 0 }].as_slice(),
            )
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

pub(super) fn allocate_managed(
    store: &RedbRecordStore,
    control: &StorageReadControl,
) -> VersionResult<RetainedTransactionAllocation> {
    store
        .receipts
        .with_admission(&store.database, control, |leases| {
            let mut owner = None;
            let id = allocate(store, true, control, |id| {
                owner = Some(leases.retain(receipt_lease_id(id), control)?);
                Ok(())
            })?;
            RetainedTransactionAllocation::retain(
                id,
                owner.expect("lease precedes managed allocation"),
            )
        })
}

pub(super) fn acknowledge(
    store: &RedbRecordStore,
    acknowledgement: ReceiptAcknowledgement,
    control: &StorageReadControl,
) -> VersionResult<()> {
    control.cancellation().check()?;
    let id = acknowledgement.transaction();
    store.check_identity(id)?;
    let transaction = physical_writer(&store.database)?;
    {
        let metadata = transaction.open_table(METADATA).map_err(redb_error)?;
        codec::validate_metadata(&metadata, store.identity)?;
        if id.allocation() > codec::read_u64(&metadata, "allocated")? {
            return Err(VersionError::UnknownTransaction);
        }
        let mut receipts = transaction.open_table(TRANSACTIONS).map_err(redb_error)?;
        acknowledgement.validate(codec::status(&receipts, id)?)?;
        if !acknowledge_row(&mut receipts, id.allocation())? {
            return Ok(());
        }
    }
    control.cancellation().check()?;
    transaction.commit().map_err(redb_error)?;
    Ok(())
}

/// Mark terminal state without changing its receipt bytes or managed ownership. Recovery alone may change a pending abandoned owner to acknowledged-aborted.
fn acknowledge_row(
    table: &mut redb::Table<'_, u64, &'static [u8]>,
    allocation: u64,
) -> VersionResult<bool> {
    let mut encoded = [0; 41];
    let length = {
        let Some(value) = table.get(allocation).map_err(redb_error)? else {
            return Ok(false);
        };
        let bytes = value.value();
        let tag = codec::receipt_tag(bytes)?;
        let state = tag & !codec::MANAGED_RECEIPT;
        if matches!(state, 3 | 4) {
            return Ok(false);
        }
        encoded[..bytes.len()].copy_from_slice(bytes);
        encoded[0] = (tag & codec::MANAGED_RECEIPT) | if state == 2 { 4 } else { 3 };
        bytes.len()
    };
    table
        .insert(allocation, &encoded[..length])
        .map_err(redb_error)?;
    Ok(true)
}

pub(super) fn reclaim(store: &RedbRecordStore, control: &StorageReadControl) -> VersionResult<u64> {
    // Persist common participant recovery first. The subsequent admission loads an unchanged durable graph, so no receipt can disappear before its final publication reference is durably released.
    store.recover_serializable_participants(control)?;
    let mut removed = 0;
    store.with_serializable_admission(control, &mut |graph, _| {
        removed = store
            .receipts
            .with_admission(&store.database, control, |leases| {
                let removed = reclaim_admitted(store, graph, leases, control)?;
                leases.reclaim();
                Ok(removed)
            })?;
        Ok(())
    })?;
    Ok(removed)
}

fn reclaim_admitted(
    store: &RedbRecordStore,
    graph: &SerializableGraph,
    leases: &dyn SerializableLeases,
    control: &StorageReadControl,
) -> VersionResult<u64> {
    let transaction = physical_writer(&store.database)?;
    let mut removed = 0;
    {
        let metadata = transaction.open_table(METADATA).map_err(redb_error)?;
        codec::validate_metadata(&metadata, store.identity)?;
        let allocated = codec::read_u64(&metadata, "allocated")?;
        let mut receipts = transaction.open_table(TRANSACTIONS).map_err(redb_error)?;
        // This fixed candidate page is charged before use; its size never depends on retained receipt count. Existing graph and lease registries keep their own controlled owners.
        let _workspace = control
            .memory()
            .reserve(std::mem::size_of::<[(u64, bool); 256]>())?;
        let mut selected = [(0, false); 256];
        let mut count = 0;
        for entry in receipts.iter().map_err(redb_error)? {
            control.cancellation().check()?;
            let (allocation, bytes) = entry.map_err(redb_error)?;
            let allocation = allocation.value();
            if allocation > allocated {
                return Err(VersionError::InvalidEncoding(
                    "receipt exceeds allocation watermark",
                ));
            }
            let id = StorageTransactionId::new(store.identity, allocation)?;
            let tag = codec::receipt_tag(bytes.value())?;
            let acknowledged = matches!(tag & !codec::MANAGED_RECEIPT, 3 | 4);
            let referenced = graph.retains_transaction_receipt(id);
            if acknowledged && referenced {
                continue;
            }
            if acknowledged
                || (tag & codec::MANAGED_RECEIPT != 0
                    && !leases.is_alive(receipt_lease_id(id), control)?)
            {
                // Decode the exact authoritative outcome before any change; owner absence never turns an unknown physical result into an abort.
                if codec::decode_status(id, bytes.value())? == CommitStatus::Unknown {
                    return Err(VersionError::UnknownTransaction);
                }
                selected[count] = (allocation, !referenced);
                count += 1;
                if count == selected.len() {
                    break;
                }
            }
        }
        for &(allocation, delete) in &selected[..count] {
            control.cancellation().check()?;
            if delete {
                receipts.remove(allocation).map_err(redb_error)?;
                removed += 1;
            } else {
                acknowledge_row(&mut receipts, allocation)?;
            }
        }
    }
    control.cancellation().check()?;
    transaction.commit().map_err(redb_error)?;
    Ok(removed)
}
