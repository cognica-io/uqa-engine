//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Row-change journal publication and leased history reclamation. Snapshot baselines retain process watermarks; atomic generation replacement preserves logical sequence numbers and keeps only the suffix needed by live readers.

use super::{
    lock_strengths_conflict, lock_would_block, FileLockCoordinator, LockStrength,
    PhysicalRowChangeTarget, RowChangeTarget, CHANGE_JOURNAL_LOCK_BYTE, CHANGE_JOURNAL_WAIT_LIMIT,
};
use std::sync::Arc;

mod codec;
mod log;
mod readers;
pub(in crate::row_locks) use readers::JournalReadLease;
pub(super) use readers::Readers;

struct JournalLock<'a>(&'a FileLockCoordinator);
impl Drop for JournalLock<'_> {
    fn drop(&mut self) {
        let _ = self
            .0
            .apply_byte_mode(CHANGE_JOURNAL_LOCK_BYTE, Some(true), None);
    }
}

impl FileLockCoordinator {
    fn lock_journal(&self) -> Result<JournalLock<'_>, String> {
        let deadline = std::time::Instant::now() + CHANGE_JOURNAL_WAIT_LIMIT;
        loop {
            match self.apply_byte_mode(CHANGE_JOURNAL_LOCK_BYTE, None, Some(true)) {
                Ok(()) => return Ok(JournalLock(self)),
                Err(error) if lock_would_block(&error) => {
                    if std::time::Instant::now() >= deadline {
                        return Err("timed out acquiring the row-change journal lock".into());
                    }
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                Err(error) => return Err(format!("acquire row-change journal lock: {error}")),
            }
        }
    }

    pub(in crate::row_locks) fn pin_change_sequence(
        self: &Arc<Self>,
    ) -> Result<(u64, Arc<JournalReadLease>), String> {
        let mut readers = self.change_journal.lock();
        let _lock = self.lock_journal()?;
        let log = log::Log::open(&self.change_path)?;
        let lease = readers.pin(self, log.next)?;
        Ok((log.next, lease))
    }

    pub(in crate::row_locks) fn publish_changes(
        &self,
        changes: &[super::PublishedRowChange],
    ) -> Result<(), String> {
        if changes.is_empty() {
            return Ok(());
        }
        let mut readers = self.change_journal.lock();
        let _lock = self.lock_journal()?;
        let mut log = log::Log::open(&self.change_path)?;
        let oldest = readers.oldest(self, log.next)?;
        log.reclaim(&self.change_path, oldest)?;
        log.append(changes)
    }

    #[cfg(test)]
    pub(in crate::row_locks) fn change_sequence(&self) -> Result<u64, String> {
        let _local = self.change_journal.lock();
        let _lock = self.lock_journal()?;
        Ok(log::Log::open(&self.change_path)?.next)
    }

    pub(super) fn detach_journal_reader(&self) {
        self.change_journal.lock().detach(self);
    }

    pub(in crate::row_locks) fn change_target_after(
        &self,
        table_hash: u64,
        doc_id: u64,
        baseline: u64,
        wanted: LockStrength,
    ) -> Result<RowChangeTarget, String> {
        Ok(
            match self.physical_change_target_after(table_hash, doc_id, baseline, wanted)? {
                PhysicalRowChangeTarget::Unchanged => RowChangeTarget::Unchanged,
                PhysicalRowChangeTarget::Present {
                    table_hash: target_table_hash,
                    doc_id,
                } if target_table_hash == table_hash => RowChangeTarget::Present(doc_id),
                PhysicalRowChangeTarget::Present { .. } | PhysicalRowChangeTarget::Deleted => {
                    RowChangeTarget::Deleted
                }
            },
        )
    }

    pub(in crate::row_locks) fn physical_change_target_after(
        &self,
        table_hash: u64,
        doc_id: u64,
        baseline: u64,
        wanted: LockStrength,
    ) -> Result<PhysicalRowChangeTarget, String> {
        let log = {
            let _local = self.change_journal.lock();
            let _lock = self.lock_journal()?;
            log::Log::open(&self.change_path)?
        };
        if baseline < log.base {
            return Err("row-change baseline no longer has a retained reader lease".into());
        }
        let next = log.next;
        if baseline >= next {
            return Ok(PhysicalRowChangeTarget::Unchanged);
        }
        let mut current = super::PublishedRowIdentity { table_hash, doc_id };
        let mut changed = false;
        for sequence in baseline..next {
            let event = log.read(sequence)?;
            if event.table_hash != current.table_hash || event.doc_id != current.doc_id {
                continue;
            }
            match event.kind {
                super::PublishedRowChangeKind::Update => {
                    changed |= lock_strengths_conflict(event.strength, wanted);
                }
                super::PublishedRowChangeKind::Delete => {
                    if lock_strengths_conflict(event.strength, wanted) {
                        return Ok(PhysicalRowChangeTarget::Deleted);
                    }
                }
                super::PublishedRowChangeKind::Rewrite(successor) => {
                    if lock_strengths_conflict(event.strength, wanted) {
                        current = successor;
                        changed = true;
                    }
                }
            }
        }
        Ok(if changed {
            PhysicalRowChangeTarget::Present {
                table_hash: current.table_hash,
                doc_id: current.doc_id,
            }
        } else {
            PhysicalRowChangeTarget::Unchanged
        })
    }
}

#[cfg(test)]
mod tests;
