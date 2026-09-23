//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Explicit acknowledgement and managed owner recovery preserve durable SSI references.

mod liveness;
#[cfg(any(windows, all(unix, not(target_os = "emscripten"))))]
pub(in crate::mvcc) use liveness::lease_file;

use rusqlite::Connection;
use uqa_core::memory::BudgetedVec;
use uqa_storage::{
    mvcc::{
        receipt_lease_id, CommitStatus, DatabaseId, ReceiptAcknowledgement,
        RetainedTransactionAllocation, SerializableGraph, SerializableLeases, StorageTransactionId,
        VersionError, VersionResult,
    },
    read_control::StorageReadControl,
};

use super::{admission, codec, native, write, PhysicalResult, SQLiteRecordStore};

impl SQLiteRecordStore {
    /// Set the shared database's maximum retained receipt count. Pending, unacknowledged and SSI-referenced receipts all count. Lowering the limit preserves existing entries and prevents allocation until release/reclamation makes room. The default is 65,536 entries; zero and values above SQLite's positive i64 range are rejected.
    pub fn set_receipt_retention_limit(
        &self,
        limit: u64,
        control: &StorageReadControl,
    ) -> VersionResult<()> {
        let limit = i64::try_from(limit).ok().filter(|limit| *limit > 0).ok_or(
            VersionError::InvalidEncoding("invalid receipt retention limit"),
        )?;
        self.with_write(control, |connection| {
            let _permit = admission::permit(connection, control)?;
            let transaction = admission::begin(connection, control)?;
            native::check_mapping(&transaction, self.native)?;
            codec::header(&transaction, self.identity)?;
            transaction.execute(
                "UPDATE _uqa_mvcc_metadata SET receipt_limit = ?1 WHERE singleton = 1",
                [limit],
            )?;
            admission::commit(transaction, control)
        })
    }

    pub(super) fn allocate_receipt_owner(
        &self,
        control: &StorageReadControl,
    ) -> VersionResult<RetainedTransactionAllocation> {
        self.with_receipt_admission(control, |leases| {
            let mut owner = None;
            let id = self.with_write(control, |connection| {
                write::allocate_with_owner(
                    connection,
                    self.identity,
                    self.native,
                    true,
                    control,
                    |id| {
                        owner = Some(leases.retain(receipt_lease_id(id), control)?);
                        Ok(())
                    },
                )
            })?;
            RetainedTransactionAllocation::retain(
                id,
                owner.expect("lease precedes managed allocation"),
            )
        })
    }

    pub(super) fn reclaim_receipts(&self, control: &StorageReadControl) -> VersionResult<u64> {
        // Finish and persist ordinary SSI recovery first. Deletion uses only a separately loaded durable graph, never the unpersisted mutations of that recovery.
        self.recover_serializable(control)?;
        let held = self.serializable_admission(control)?;
        self.with_receipt_admission(control, |leases| {
            let removed = self.with_write(control, |connection| {
                reclaim(
                    connection,
                    self.identity,
                    self.native,
                    held.graph(),
                    leases,
                    control,
                )
            })?;
            leases.reclaim();
            Ok(removed)
        })
    }
}

pub(super) fn acknowledge(
    connection: &Connection,
    native: Option<native::NativeRecordNamespace>,
    acknowledgement: ReceiptAcknowledgement,
    control: &StorageReadControl,
) -> PhysicalResult<()> {
    control.check().map_err(VersionError::from)?;
    let _permit = admission::permit(connection, control)?;
    let transaction = admission::begin(connection, control)?;
    native::check_mapping(&transaction, native)?;
    let id = acknowledgement.transaction();
    let header = codec::header(&transaction, id.database())?;
    if id.allocation() > header.allocated {
        return Err(VersionError::UnknownTransaction.into());
    }
    acknowledgement.validate(codec::status(&transaction, id)?)?;
    transaction.execute("UPDATE _uqa_mvcc_transactions SET status = CASE status WHEN 1 THEN 3 WHEN 2 THEN 4 ELSE status END WHERE allocation = ?1", [id.allocation().to_be_bytes().as_slice()])?;
    admission::commit(transaction, control)
}

/// SSI and receipt admission remain held across this atomic main write. Authoritatively dead managed owners release their retry rights; manual owners never do so implicitly. Their original physical state still decides commit/abort, and every retained graph reference remains protected.
fn reclaim(
    connection: &Connection,
    identity: DatabaseId,
    native: Option<native::NativeRecordNamespace>,
    graph: &SerializableGraph,
    leases: &dyn SerializableLeases,
    control: &StorageReadControl,
) -> PhysicalResult<u64> {
    let _permit = admission::permit(connection, control)?;
    let transaction = admission::begin(connection, control)?;
    native::check_mapping(&transaction, native)?;
    let header = codec::header(&transaction, identity)?;
    let mut selected = BudgetedVec::new(control.memory());
    {
        let mut statement = transaction.prepare("SELECT allocation, status, managed FROM _uqa_mvcc_transactions WHERE status IN (3, 4) OR managed = 1 ORDER BY allocation")?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            control.check().map_err(VersionError::from)?;
            let allocation = codec::integer(codec::bytes(row, 0)?)?;
            if allocation > header.allocated {
                return Err(
                    VersionError::InvalidEncoding("receipt exceeds allocation watermark").into(),
                );
            }
            let id = StorageTransactionId::new(identity, allocation)?;
            let status: i64 = row.get(1)?;
            let acknowledged = matches!(status, 3 | 4);
            if acknowledged && graph.retains_transaction_receipt(id) {
                continue;
            }
            let managed: bool = row.get(2)?;
            if acknowledged || (managed && !leases.is_alive(receipt_lease_id(id), control)?) {
                // Decode authoritative status before preparing any change, including rejecting corruption. Unknown can never be inferred from owner death.
                let status = codec::status(&transaction, id)?;
                if status == CommitStatus::Unknown {
                    return Err(VersionError::UnknownTransaction.into());
                }
                selected
                    .push((
                        allocation.to_be_bytes(),
                        !graph.retains_transaction_receipt(id),
                        matches!(status, CommitStatus::Committed(_)),
                    ))
                    .map_err(VersionError::from)?;
                if selected.len() == 256 {
                    break;
                }
            }
        }
    }
    let mut removed = 0;
    for (allocation, delete, committed) in selected.iter() {
        control.check().map_err(VersionError::from)?;
        if *delete {
            transaction.execute(
                "DELETE FROM _uqa_mvcc_transactions WHERE allocation = ?1",
                [allocation.as_slice()],
            )?;
            removed += 1;
        } else {
            transaction.execute(
                "UPDATE _uqa_mvcc_transactions SET status = ?2 WHERE allocation = ?1",
                rusqlite::params![allocation.as_slice(), if *committed { 4 } else { 3 }],
            )?;
        }
    }
    admission::commit(transaction, control)?;
    Ok(removed)
}
