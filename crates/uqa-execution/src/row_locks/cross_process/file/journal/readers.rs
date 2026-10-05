//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One oldest-live-reader slot per process, protected by an OS liveness lock.

use std::collections::BTreeMap;
use std::sync::Arc;

use uqa_storage::native_file::{try_lock_byte, unlock_byte};

use super::super::{
    lock_would_block, read_exact_at, write_all_at, FileLockCoordinator, HOLDER_SLOT_BASE,
    HOLDER_SLOT_COUNT, HOLDER_SLOT_SIZE,
};

const READER_BASE: u64 = HOLDER_SLOT_BASE + HOLDER_SLOT_COUNT * HOLDER_SLOT_SIZE;
const READER_LIMIT: u64 = super::super::super::RELATION_BASE - READER_BASE;
const SLOT_BYTES: u64 = 16;

#[derive(Default)]
pub(in super::super) struct Readers {
    counts: BTreeMap<u64, usize>,
    slot: Option<u64>,
    advertised: Option<u64>,
}

/// Clones of a snapshot share this lease; only its last owner advances the process watermark.
pub(in crate::row_locks) struct JournalReadLease {
    coordinator: Arc<FileLockCoordinator>,
    sequence: u64,
}

impl std::fmt::Debug for JournalReadLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JournalReadLease")
            .field("sequence", &self.sequence)
            .finish_non_exhaustive()
    }
}

impl Drop for JournalReadLease {
    fn drop(&mut self) {
        let coordinator = &self.coordinator;
        let mut readers = coordinator.change_journal.lock();
        readers.remove(self.sequence);
        // A failed watermark write leaves either the old value or an invalid slot, which prevents reclamation. A later publication retries it; process exit releases the liveness lock in every case.
        let _ = (|| {
            let _lock = coordinator.lock_journal()?;
            readers.advertise(coordinator)?;
            let mut log = super::log::Log::open(&coordinator.change_path)?;
            let oldest = readers.oldest(coordinator, log.next)?;
            log.reclaim(&coordinator.change_path, oldest)
        })();
    }
}

impl Readers {
    fn first(&self) -> Option<u64> {
        self.counts.first_key_value().map(|(&sequence, _)| sequence)
    }

    fn remove(&mut self, sequence: u64) {
        let count = self
            .counts
            .get_mut(&sequence)
            .expect("a live journal lease");
        *count -= 1;
        if *count == 0 {
            self.counts.remove(&sequence);
        }
    }

    pub(super) fn pin(
        &mut self,
        coordinator: &Arc<FileLockCoordinator>,
        sequence: u64,
    ) -> Result<Arc<JournalReadLease>, String> {
        *self.counts.entry(sequence).or_default() += 1;
        if let Err(error) = self.advertise(coordinator) {
            self.remove(sequence);
            return Err(error);
        }
        Ok(Arc::new(JournalReadLease {
            coordinator: Arc::clone(coordinator),
            sequence,
        }))
    }

    pub(super) fn advertise(&mut self, coordinator: &FileLockCoordinator) -> Result<(), String> {
        if self.slot.is_none() && self.first().is_some() {
            let slots = coordinator.change_readers.metadata().map_err(io)?.len() / SLOT_BYTES;
            for slot in 0..=slots.min(READER_LIMIT.saturating_sub(1)) {
                match try_lock_byte(&coordinator.file, READER_BASE + slot, true) {
                    Ok(()) => {
                        self.slot = Some(slot);
                        break;
                    }
                    Err(error) if lock_would_block(&error) => {}
                    Err(error) => return Err(io(error)),
                }
            }
            if self.slot.is_none() {
                return Err("row-change journal reader slots exhausted".into());
            }
        }
        if let Some(slot) = self.slot {
            let stored = match self.first() {
                Some(sequence) => sequence
                    .checked_add(1)
                    .ok_or("row-change reader sequence exhausted")?,
                None => 0,
            };
            if self.advertised == Some(stored) {
                return Ok(());
            }
            let mut record = [0; SLOT_BYTES as usize];
            record[..8].copy_from_slice(&stored.to_be_bytes());
            record[8..].copy_from_slice(&(!stored).to_be_bytes());
            write_all_at(&coordinator.change_readers, &record, slot * SLOT_BYTES).map_err(io)?;
            self.advertised = Some(stored);
        }
        Ok(())
    }

    /// A stale slot cannot retain history after its process dies, even when its PID is reused.
    pub(super) fn oldest(
        &mut self,
        coordinator: &FileLockCoordinator,
        next: u64,
    ) -> Result<u64, String> {
        self.advertise(coordinator)?;
        let mut oldest = self.first().unwrap_or(next);
        let bytes = coordinator.change_readers.metadata().map_err(io)?.len();
        let slots = bytes.div_ceil(SLOT_BYTES);
        if slots > READER_LIMIT {
            return Err("row-change reader registry exceeds its liveness range".into());
        }
        for slot in 0..slots {
            if Some(slot) == self.slot {
                continue;
            }
            match try_lock_byte(&coordinator.file, READER_BASE + slot, true) {
                Ok(()) => {
                    unlock_byte(&coordinator.file, READER_BASE + slot).map_err(io)?;
                }
                Err(error) if lock_would_block(&error) => {
                    let mut value = [0; SLOT_BYTES as usize];
                    read_exact_at(&coordinator.change_readers, &mut value, slot * SLOT_BYTES)
                        .map_err(io)?;
                    let stored = decode(&value)?;
                    if let Some(sequence) = stored.checked_sub(1) {
                        oldest = oldest.min(sequence);
                    }
                }
                Err(error) => return Err(io(error)),
            }
        }
        Ok(oldest)
    }

    pub(super) fn detach(&mut self, coordinator: &FileLockCoordinator) {
        if let Some(slot) = self.slot.take() {
            let _ = unlock_byte(&coordinator.file, READER_BASE + slot);
        }
    }
}

fn decode(record: &[u8; SLOT_BYTES as usize]) -> Result<u64, String> {
    let value = u64::from_be_bytes(record[..8].try_into().expect("reader sequence width"));
    let complement = u64::from_be_bytes(record[8..].try_into().expect("reader complement width"));
    if value != !complement {
        return Err("row-change reader watermark is incomplete".into());
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_torn_watermark_never_advertises_a_later_baseline() {
        for (previous, next) in [(0_u64, 1_u64), (255, 256), (65535, 65536), (65536, 0)] {
            let record = |value: u64| {
                let mut bytes = [0; SLOT_BYTES as usize];
                bytes[..8].copy_from_slice(&value.to_be_bytes());
                bytes[8..].copy_from_slice(&(!value).to_be_bytes());
                bytes
            };
            for written in 0..=SLOT_BYTES as usize {
                let mut torn = record(previous);
                torn[..written].copy_from_slice(&record(next)[..written]);
                if let Ok(value) = decode(&torn) {
                    assert!(value == previous || value == next);
                }
            }
        }
    }
}

fn io(error: std::io::Error) -> String {
    format!("row-change reader registry I/O: {error}")
}
