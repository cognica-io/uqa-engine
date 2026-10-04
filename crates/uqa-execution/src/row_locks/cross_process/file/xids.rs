//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable transaction ID allocation.
//!
//! Every transaction ID handed out lies below a limit that was made durable first, so an identifier is never issued twice even after a machine failure. A reservation moves the limit ahead of several identifiers at once, and a cursor written without its own sync records the next identifier inside the reservation. The cursor survives the failure of any process; only a machine failure can lose its latest value.
//!
//! Every process that allocates holds the attachment byte shared for as long as it keeps the sidecar open. A process that can claim the byte exclusively is the only one attached and cannot tell an orderly restart from a machine failure, so it discards the cursor and continues at the durable limit. A process that finds another holder inherits a cursor that holder has kept current. Allocation is serialized by the allocator byte, which also serializes attachment.

use super::{
    lock_would_block, read_exact_at, write_all_at, FileLockCoordinator, CHANGE_JOURNAL_WAIT_LIMIT,
    TRANSACTION_XID_ATTACHMENT_BYTE, TRANSACTION_XID_CURSOR_MAGIC, TRANSACTION_XID_CURSOR_OFFSET,
    TRANSACTION_XID_CURSOR_SIZE, TRANSACTION_XID_CURSOR_VERSION, TRANSACTION_XID_LOCK_BYTE,
    TRANSACTION_XID_STATE_MAGIC, TRANSACTION_XID_STATE_OFFSET, TRANSACTION_XID_STATE_SIZE,
    TRANSACTION_XID_STATE_VERSION,
};

/// The first transaction ID of a new database, and the one that follows the last.
const FIRST_TRANSACTION_XID: u32 = 3;

/// Transaction IDs one durable reservation covers at most. A coordinator reserves one identifier first and doubles each later reservation, so a process that ends without returning its reservation wastes fewer identifiers than it used.
const MAXIMUM_RESERVATION: u32 = 1024;

/// One coordinator's part in allocation, guarded by its allocator mutex.
pub(super) struct TransactionXids {
    /// Whether this coordinator holds the attachment byte.
    attached: bool,
    /// Transaction IDs its next durable reservation covers.
    reservation: u32,
}

impl TransactionXids {
    pub(super) const fn new() -> Self {
        Self {
            attached: false,
            reservation: 1,
        }
    }
}

/// The transaction ID after `xid`.
const fn following(xid: u32) -> u32 {
    if xid == u32::MAX {
        FIRST_TRANSACTION_XID
    } else {
        xid + 1
    }
}

/// The limit of a reservation of at most `count` transaction IDs starting at `first`. A reservation ends with the last transaction ID, after which the sequence starts over.
const fn reservation_limit(first: u32, count: u32) -> u32 {
    match first.checked_add(count) {
        Some(limit) => limit,
        None => FIRST_TRANSACTION_XID,
    }
}

impl FileLockCoordinator {
    /// Allocate one database-wide normal transaction ID. The durable limit and the shared cursor make allocations unique and ascending across processes opening the same database, including after the database is reopened.
    pub(in crate::row_locks) fn allocate_transaction_xid(&self) -> Result<Option<u32>, String> {
        let mut xids = self.transaction_xids.lock();
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
        let allocation = self.allocate_locked(&mut xids);
        let unlock = self
            .apply_byte_mode(TRANSACTION_XID_LOCK_BYTE, Some(true), None)
            .map_err(|error| format!("release transaction XID allocator lock failed: {error}"));
        match (allocation, unlock) {
            (Ok(xid), Ok(())) => Ok(Some(xid)),
            (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
            (Err(error), Err(unlock_error)) => Err(format!("{error}; {unlock_error}")),
        }
    }

    fn allocate_locked(&self, xids: &mut TransactionXids) -> Result<u32, String> {
        let length = self
            .file
            .metadata()
            .map_err(|error| format!("read transaction XID state length failed: {error}"))?
            .len();
        let mut limit = self.transaction_xid_limit(length)?;
        if !xids.attached {
            self.attach_transaction_xids(length)?;
            xids.attached = true;
        }
        let next = self.transaction_xid_cursor(length, limit)?.unwrap_or(limit);
        if next == limit {
            // The reservation is used up, or nothing vouches for its cursor. The new limit is durable before any identifier below it is handed out.
            limit = reservation_limit(next, xids.reservation);
            self.write_transaction_xid_limit(limit)?;
            self.file
                .sync_data()
                .map_err(|error| format!("sync transaction XID state failed: {error}"))?;
            xids.reservation = xids.reservation.saturating_mul(2).min(MAXIMUM_RESERVATION);
        }
        let mut cursor = [0_u8; TRANSACTION_XID_CURSOR_SIZE];
        cursor[0..4].copy_from_slice(&TRANSACTION_XID_CURSOR_MAGIC.to_be_bytes());
        cursor[4..8].copy_from_slice(&TRANSACTION_XID_CURSOR_VERSION.to_be_bytes());
        cursor[8..16].copy_from_slice(&u64::from(following(next)).to_be_bytes());
        cursor[16..24].copy_from_slice(&u64::from(limit).to_be_bytes());
        write_all_at(&self.file, &cursor, TRANSACTION_XID_CURSOR_OFFSET)
            .map_err(|error| format!("write transaction XID cursor failed: {error}"))?;
        Ok(next)
    }

    /// The first transaction ID that no durable reservation covers. A sidecar written before reservations existed stores its next transaction ID here, which is the limit of an empty reservation.
    fn transaction_xid_limit(&self, length: u64) -> Result<u32, String> {
        let state_end = TRANSACTION_XID_STATE_OFFSET
            + u64::try_from(TRANSACTION_XID_STATE_SIZE)
                .expect("transaction XID state size fits u64");
        if length < state_end {
            return Ok(FIRST_TRANSACTION_XID);
        }
        let mut state = [0_u8; TRANSACTION_XID_STATE_SIZE];
        read_exact_at(&self.file, &mut state, TRANSACTION_XID_STATE_OFFSET)
            .map_err(|error| format!("read transaction XID state failed: {error}"))?;
        if state.iter().all(|byte| *byte == 0) {
            return Ok(FIRST_TRANSACTION_XID);
        }
        let magic =
            u32::from_be_bytes(state[0..4].try_into().expect("transaction XID magic width"));
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
        if magic != TRANSACTION_XID_STATE_MAGIC || version != TRANSACTION_XID_STATE_VERSION {
            return Err("transaction XID allocator state is corrupt".to_string());
        }
        u32::try_from(stored)
            .ok()
            .filter(|limit| *limit >= FIRST_TRANSACTION_XID)
            .ok_or_else(|| "transaction XID allocator state is corrupt".to_string())
    }

    fn write_transaction_xid_limit(&self, limit: u32) -> Result<(), String> {
        let mut state = [0_u8; TRANSACTION_XID_STATE_SIZE];
        state[0..4].copy_from_slice(&TRANSACTION_XID_STATE_MAGIC.to_be_bytes());
        state[4..8].copy_from_slice(&TRANSACTION_XID_STATE_VERSION.to_be_bytes());
        state[8..16].copy_from_slice(&u64::from(limit).to_be_bytes());
        write_all_at(&self.file, &state, TRANSACTION_XID_STATE_OFFSET)
            .map_err(|error| format!("write transaction XID state failed: {error}"))
    }

    /// The cursor's next transaction ID when the cursor belongs to the reservation that ends at `limit`. A cursor of any other reservation, or one that is not a cursor at all, is no cursor.
    fn transaction_xid_cursor(&self, length: u64, limit: u32) -> Result<Option<u32>, String> {
        let cursor_end = TRANSACTION_XID_CURSOR_OFFSET
            + u64::try_from(TRANSACTION_XID_CURSOR_SIZE)
                .expect("transaction XID cursor size fits u64");
        if length < cursor_end {
            return Ok(None);
        }
        let mut cursor = [0_u8; TRANSACTION_XID_CURSOR_SIZE];
        read_exact_at(&self.file, &mut cursor, TRANSACTION_XID_CURSOR_OFFSET)
            .map_err(|error| format!("read transaction XID cursor failed: {error}"))?;
        let magic = u32::from_be_bytes(
            cursor[0..4]
                .try_into()
                .expect("transaction XID cursor magic width"),
        );
        let version = u32::from_be_bytes(
            cursor[4..8]
                .try_into()
                .expect("transaction XID cursor version width"),
        );
        let next = u64::from_be_bytes(
            cursor[8..16]
                .try_into()
                .expect("transaction XID cursor value width"),
        );
        let bound = u64::from_be_bytes(
            cursor[16..24]
                .try_into()
                .expect("transaction XID cursor limit width"),
        );
        if magic != TRANSACTION_XID_CURSOR_MAGIC
            || version != TRANSACTION_XID_CURSOR_VERSION
            || bound != u64::from(limit)
        {
            return Ok(None);
        }
        Ok(u32::try_from(next)
            .ok()
            .filter(|next| *next >= FIRST_TRANSACTION_XID))
    }

    /// Join the processes that keep the cursor current, discarding the cursor when there are none. The caller holds the allocator lock, so every other holder of the attachment byte has finished attaching.
    fn attach_transaction_xids(&self, length: u64) -> Result<(), String> {
        match self.apply_byte_mode(TRANSACTION_XID_ATTACHMENT_BYTE, None, Some(true)) {
            Ok(()) => {
                let attached = self.discard_transaction_xid_cursor(length).and_then(|()| {
                    self.apply_byte_mode(TRANSACTION_XID_ATTACHMENT_BYTE, Some(true), Some(false))
                        .map_err(|error| {
                            format!("share transaction XID attachment failed: {error}")
                        })
                });
                if attached.is_err() {
                    // The byte may still be claimed exclusively, which would keep every later attachment from sharing it.
                    let _ = self.apply_byte_mode(TRANSACTION_XID_ATTACHMENT_BYTE, Some(true), None);
                }
                attached
            }
            Err(error) if lock_would_block(&error) => self
                .apply_byte_mode(TRANSACTION_XID_ATTACHMENT_BYTE, None, Some(false))
                .map_err(|error| format!("share transaction XID attachment failed: {error}")),
            Err(error) => Err(format!("claim transaction XID attachment failed: {error}")),
        }
    }

    fn discard_transaction_xid_cursor(&self, length: u64) -> Result<(), String> {
        if length <= TRANSACTION_XID_CURSOR_OFFSET {
            return Ok(());
        }
        write_all_at(
            &self.file,
            &[0_u8; TRANSACTION_XID_CURSOR_SIZE],
            TRANSACTION_XID_CURSOR_OFFSET,
        )
        .map_err(|error| format!("discard transaction XID cursor failed: {error}"))
    }

    /// Return the unused part of the reservation when this coordinator is the last one attached, so that an orderly restart continues at the next transaction ID. The lowered limit needs no sync: the durable limit it replaces is higher, and the next reservation makes its own limit durable before handing out an identifier.
    pub(super) fn detach_transaction_xids(&mut self) {
        if !self.transaction_xids.get_mut().attached {
            return;
        }
        // A process holding the allocator byte is allocating, and so is attached: this coordinator is not the last one and returns nothing.
        if self
            .apply_byte_mode(TRANSACTION_XID_LOCK_BYTE, None, Some(true))
            .is_err()
        {
            return;
        }
        if self
            .apply_byte_mode(TRANSACTION_XID_ATTACHMENT_BYTE, Some(false), Some(true))
            .is_ok()
        {
            let _ = self.return_transaction_xid_reservation();
            let _ = self.apply_byte_mode(TRANSACTION_XID_ATTACHMENT_BYTE, Some(true), None);
        } else {
            let _ = self.apply_byte_mode(TRANSACTION_XID_ATTACHMENT_BYTE, Some(false), None);
        }
        self.transaction_xids.get_mut().attached = false;
        let _ = self.apply_byte_mode(TRANSACTION_XID_LOCK_BYTE, Some(true), None);
    }

    fn return_transaction_xid_reservation(&self) -> Result<(), String> {
        let length = self
            .file
            .metadata()
            .map_err(|error| format!("read transaction XID state length failed: {error}"))?
            .len();
        let limit = self.transaction_xid_limit(length)?;
        match self.transaction_xid_cursor(length, limit)? {
            Some(next) if next != limit => self.write_transaction_xid_limit(next),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests;
