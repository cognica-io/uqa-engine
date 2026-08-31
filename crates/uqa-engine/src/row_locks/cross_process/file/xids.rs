//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable transaction ID allocation.

use super::{
    lock_would_block, read_exact_at, write_all_at, FileLockCoordinator, CHANGE_JOURNAL_WAIT_LIMIT,
    TRANSACTION_XID_LOCK_BYTE, TRANSACTION_XID_STATE_MAGIC, TRANSACTION_XID_STATE_OFFSET,
    TRANSACTION_XID_STATE_SIZE, TRANSACTION_XID_STATE_VERSION,
};

impl FileLockCoordinator {
    /// Allocate one database-wide normal transaction ID. The durable sidecar state and native record lock make allocations unique across processes opening the same database, including after the database is reopened.
    pub(in crate::row_locks) fn allocate_transaction_xid(&self) -> Result<Option<u32>, String> {
        let _guard = self.transaction_xids.lock();
        let deadline = std::time::Instant::now() + CHANGE_JOURNAL_WAIT_LIMIT;
        loop {
            match self.apply_byte_mode(TRANSACTION_XID_LOCK_BYTE, None, Some(true)) {
                Ok(()) => break,
                Err(error) if lock_would_block(&error) => {
                    if std::time::Instant::now() >= deadline {
                        return Err(format!(
                                "timed out after {} seconds acquiring the transaction XID allocator lock",
                                CHANGE_JOURNAL_WAIT_LIMIT.as_secs()
                            ));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                Err(error) => {
                    return Err(format!(
                        "acquire transaction XID allocator lock failed: {error}"
                    ));
                }
            }
        }
        let allocation = (|| {
            let mut state = [0_u8; TRANSACTION_XID_STATE_SIZE];
            let state_end = TRANSACTION_XID_STATE_OFFSET
                + u64::try_from(TRANSACTION_XID_STATE_SIZE)
                    .expect("transaction XID state size fits u64");
            let next = if self
                .file
                .metadata()
                .map_err(|error| format!("read transaction XID state length failed: {error}"))?
                .len()
                < state_end
            {
                3_u32
            } else {
                read_exact_at(&self.file, &mut state, TRANSACTION_XID_STATE_OFFSET)
                    .map_err(|error| format!("read transaction XID state failed: {error}"))?;
                if state.iter().all(|byte| *byte == 0) {
                    3_u32
                } else {
                    let magic = u32::from_be_bytes(
                        state[0..4].try_into().expect("transaction XID magic width"),
                    );
                    let version = u32::from_be_bytes(
                        state[4..8]
                            .try_into()
                            .expect("transaction XID version width"),
                    );
                    let stored = u64::from_be_bytes(
                        state[8..16]
                            .try_into()
                            .expect("transaction XID value width"),
                    );
                    if magic != TRANSACTION_XID_STATE_MAGIC
                        || version != TRANSACTION_XID_STATE_VERSION
                        || !(3..=u64::from(u32::MAX)).contains(&stored)
                    {
                        return Err("transaction XID allocator state is corrupt".to_string());
                    }
                    u32::try_from(stored).expect("validated transaction XID fits into u32")
                }
            };
            let following = if next == u32::MAX { 3 } else { next + 1 };
            state[0..4].copy_from_slice(&TRANSACTION_XID_STATE_MAGIC.to_be_bytes());
            state[4..8].copy_from_slice(&TRANSACTION_XID_STATE_VERSION.to_be_bytes());
            state[8..16].copy_from_slice(&u64::from(following).to_be_bytes());
            write_all_at(&self.file, &state, TRANSACTION_XID_STATE_OFFSET)
                .map_err(|error| format!("write transaction XID state failed: {error}"))?;
            self.file
                .sync_data()
                .map_err(|error| format!("sync transaction XID state failed: {error}"))?;
            Ok(Some(next))
        })();
        let unlock = self
            .apply_byte_mode(TRANSACTION_XID_LOCK_BYTE, Some(true), None)
            .map_err(|error| format!("release transaction XID allocator lock failed: {error}"));
        match (allocation, unlock) {
            (Ok(xid), Ok(())) => Ok(xid),
            (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
            (Err(error), Err(unlock_error)) => Err(format!("{error}; {unlock_error}")),
        }
    }
}
