//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded managed allocations retain fully durable Pending receipts and leases.

use std::sync::Arc;

use parking_lot::Mutex;
use uqa_core::memory::MemoryBudget;
use uqa_storage::{
    mvcc::{
        receipt_lease_id, CommitStatus, RetainedTransactionAllocation, SerializableParticipant,
        StorageTransactionId, VersionError, VersionResult,
    },
    read_control::StorageReadControl,
};

use super::super::{admission, codec, write, SQLiteRecordStore};

const BATCH_SIZE: usize = 16;
const RETAINED_BYTES: usize = 64 * 1024;

#[cfg(test)]
mod tests;

struct Reserved {
    leases: [Option<SerializableParticipant>; BATCH_SIZE],
    next: usize,
    end: usize,
    desired: usize,
}

impl Default for Reserved {
    fn default() -> Self {
        Self {
            leases: std::array::from_fn(|_| None),
            next: 0,
            end: 0,
            desired: 1,
        }
    }
}

struct Pool {
    reserved: Mutex<Reserved>,
    memory: MemoryBudget,
}

/// Cloned stores share one small reserve. Active owners retain their reserve
/// charge; when its allowance is occupied, ordinary single allocation remains
/// available under the requesting transaction's allowance.
#[derive(Clone)]
pub(in crate::mvcc) struct ManagedAllocations(Arc<Pool>);

impl Default for ManagedAllocations {
    fn default() -> Self {
        Self(Arc::new(Pool {
            reserved: Mutex::new(Reserved::default()),
            memory: MemoryBudget::new(RETAINED_BYTES),
        }))
    }
}

fn retain_for_caller(
    id: StorageTransactionId,
    lease: &SerializableParticipant,
    control: &StorageReadControl,
) -> VersionResult<RetainedTransactionAllocation> {
    let lease = SerializableParticipant::retain(lease.id(), Box::new(lease.clone()), control)?;
    RetainedTransactionAllocation::retain(id, lease)
}

impl ManagedAllocations {
    pub(super) fn allocate(
        &self,
        store: &SQLiteRecordStore,
        control: &StorageReadControl,
    ) -> VersionResult<RetainedTransactionAllocation> {
        control.check()?;
        let mut reserved = loop {
            if let Some(reserved) = self.0.reserved.try_lock() {
                break reserved;
            }
            admission::wait(control).map_err(super::super::Error::into_version)?;
        };
        if reserved.next < reserved.end {
            let index = reserved.next;
            let lease = reserved.leases[index].as_ref().expect("reserved lease");
            let id = StorageTransactionId::new(store.identity, lease.id().allocation())?;
            // A reserve cannot authorize a replaced/restored database or turn a
            // receipt changed through a raw physical API back into Pending.
            store.read(|connection| {
                if codec::status(connection, id)? != CommitStatus::Pending {
                    return Err(VersionError::TransactionSealed.into());
                }
                Ok(())
            })?;
            control.check()?;
            let owner = retain_for_caller(id, lease, control)?;
            reserved.leases[index] = None;
            reserved.next += 1;
            return Ok(owner);
        }

        let retained = StorageReadControl::new(&self.0.memory, control.cancellation());
        let mut batch = Reserved::default();
        let mut owner = None;
        let allocation = store.with_receipt_capacity(control, |leases, capacity| {
            store.with_write(control, |connection| {
                write::allocate_batch(
                    connection,
                    store.identity,
                    store.native,
                    true,
                    capacity.clamp(1, reserved.desired) as u64,
                    control,
                    |id| {
                        let lease = leases.retain(receipt_lease_id(id), &retained)?;
                        if batch.end == 0 {
                            owner = Some(retain_for_caller(id, &lease, control)?);
                        } else {
                            batch.leases[batch.end] = Some(lease);
                        }
                        batch.end += 1;
                        Ok(())
                    },
                )
            })
        });
        match allocation {
            Ok(_) => {
                batch.next = 1;
                batch.desired = (reserved.desired * 2).min(BATCH_SIZE);
                *reserved = batch;
                Ok(owner.expect("first durable allocation has its caller"))
            }
            Err(VersionError::Memory(_)) => {
                // The failed batch rolled back before exposing any identity.
                // Release its leases before retrying the single-owner path.
                drop(batch);
                drop(owner);
                store.allocate_one_receipt_owner(control)
            }
            Err(error) => Err(error),
        }
    }
}
