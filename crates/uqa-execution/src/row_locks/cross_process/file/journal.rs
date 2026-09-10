//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable row-change journal publication, lookup, and codec.

use super::{
    lock_strengths_conflict, lock_would_block, read_exact_at, write_all_at, FileLockCoordinator,
    LockStrength, PhysicalRowChangeTarget, RowChangeTarget, CHANGE_ENTRY_MAGIC, CHANGE_ENTRY_SIZE,
    CHANGE_JOURNAL_LOCK_BYTE, CHANGE_JOURNAL_WAIT_LIMIT,
};

impl FileLockCoordinator {
    /// Append committed tuple-version events to an unbounded sidecar journal. Entries are never overwritten, so a long-lived statement cannot lose the generation history needed to distinguish an update chain from a delete followed by primary-key reuse.
    pub(in crate::row_locks) fn publish_changes(
        &self,
        changes: &[super::PublishedRowChange],
    ) -> Result<(), String> {
        if changes.is_empty() {
            return Ok(());
        }
        let _guard = self.change_journal.lock();
        let deadline = std::time::Instant::now() + CHANGE_JOURNAL_WAIT_LIMIT;
        loop {
            match self.apply_byte_mode(CHANGE_JOURNAL_LOCK_BYTE, None, Some(true)) {
                Ok(()) => break,
                Err(error) if lock_would_block(&error) => {
                    if std::time::Instant::now() >= deadline {
                        return Err(format!(
                            "timed out after {} seconds acquiring the row-change journal lock",
                            CHANGE_JOURNAL_WAIT_LIMIT.as_secs()
                        ));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                Err(error) => {
                    return Err(format!("acquire row-change journal lock failed: {error}"));
                }
            }
        }
        let mut original_len = None;
        let publication = (|| {
            let mut next = self.change_sequence_unlocked()?;
            let journal_len = next.checked_mul(CHANGE_ENTRY_SIZE).ok_or_else(|| {
                "row-change journal byte length overflow before publication".to_string()
            })?;
            original_len = Some(journal_len);
            for change in changes {
                let offset = next.checked_mul(CHANGE_ENTRY_SIZE).ok_or_else(|| {
                    "row-change journal byte offset overflow during publication".to_string()
                })?;
                let entry = encode_change_entry(next, change);
                write_all_at(&self.change_file, &entry, offset).map_err(|error| {
                    format!("write row-change journal entry {next} failed: {error}")
                })?;
                next = next.checked_add(1).ok_or_else(|| {
                    "row-change journal sequence overflow during publication".to_string()
                })?;
            }
            self.change_file
                .sync_data()
                .map_err(|error| format!("sync row-change journal failed: {error}"))
        })();
        let publication = match (publication, original_len) {
            (Err(error), Some(original_len)) => {
                let rollback = self
                        .change_file
                        .set_len(original_len)
                        .and_then(|()| self.change_file.sync_data())
                        .map_err(|rollback_error| {
                            format!(
                                "{error}; restore row-change journal to {original_len} bytes failed: {rollback_error}"
                            )
                        });
                rollback.and(Err(error))
            }
            (result, _) => result,
        };
        let unlock = self
            .apply_byte_mode(CHANGE_JOURNAL_LOCK_BYTE, Some(true), None)
            .map_err(|error| format!("release row-change journal lock failed: {error}"));
        match (publication, unlock) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(error), Err(unlock_error)) => Err(format!("{error}; {unlock_error}")),
        }
    }

    fn change_sequence_unlocked(&self) -> Result<u64, String> {
        let bytes = self
            .change_file
            .metadata()
            .map_err(|error| format!("read row-change journal length failed: {error}"))?
            .len();
        if bytes % CHANGE_ENTRY_SIZE != 0 {
            return Err(format!(
                "row-change journal length {bytes} is not a multiple of {CHANGE_ENTRY_SIZE}"
            ));
        }
        Ok(bytes / CHANGE_ENTRY_SIZE)
    }

    /// Sequence immediately after the newest committed tuple-version event.
    pub(in crate::row_locks) fn change_sequence(&self) -> Result<u64, String> {
        self.change_sequence_unlocked()
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
        let next = self.change_sequence()?;
        if baseline >= next {
            return Ok(PhysicalRowChangeTarget::Unchanged);
        }
        let mut current = super::PublishedRowIdentity { table_hash, doc_id };
        let mut changed = false;
        for sequence in baseline..next {
            let offset = sequence.saturating_mul(CHANGE_ENTRY_SIZE);
            let mut entry = [0_u8; CHANGE_ENTRY_SIZE as usize];
            read_exact_at(&self.change_file, &mut entry, offset).map_err(|error| {
                format!("read row-change journal entry {sequence} failed: {error}")
            })?;
            let event = decode_change_entry(sequence, &entry)?;
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

fn encode_change_entry(
    sequence: u64,
    change: &super::PublishedRowChange,
) -> [u8; CHANGE_ENTRY_SIZE as usize] {
    let mut entry = [0_u8; CHANGE_ENTRY_SIZE as usize];
    entry[0..4].copy_from_slice(&CHANGE_ENTRY_MAGIC.to_be_bytes());
    let (kind, successor) = match change.kind {
        super::PublishedRowChangeKind::Update => (
            1,
            super::PublishedRowIdentity {
                table_hash: 0,
                doc_id: 0,
            },
        ),
        super::PublishedRowChangeKind::Delete => (
            2,
            super::PublishedRowIdentity {
                table_hash: 0,
                doc_id: 0,
            },
        ),
        super::PublishedRowChangeKind::Rewrite(successor) => (3, successor),
    };
    entry[4] = kind;
    entry[5] = strength_code(change.strength);
    entry[8..16].copy_from_slice(&sequence.wrapping_add(1).to_be_bytes());
    entry[16..24].copy_from_slice(&change.table_hash.to_be_bytes());
    entry[24..32].copy_from_slice(&change.doc_id.to_be_bytes());
    entry[32..40].copy_from_slice(&successor.doc_id.to_be_bytes());
    entry[40..48].copy_from_slice(&successor.table_hash.to_be_bytes());
    entry
}

fn decode_change_entry(
    sequence: u64,
    entry: &[u8; CHANGE_ENTRY_SIZE as usize],
) -> Result<super::PublishedRowChange, String> {
    if entry[0..4] != CHANGE_ENTRY_MAGIC.to_be_bytes() {
        return Err(format!(
            "row-change journal entry {sequence} has invalid magic"
        ));
    }
    let stored_sequence = u64::from_be_bytes(
        entry[8..16]
            .try_into()
            .map_err(|_| format!("decode row-change journal sequence for entry {sequence}"))?,
    );
    if stored_sequence != sequence.wrapping_add(1) {
        return Err(format!(
            "row-change journal entry {sequence} changed while it was read"
        ));
    }
    let table_hash = u64::from_be_bytes(
        entry[16..24]
            .try_into()
            .map_err(|_| format!("decode row-change table for entry {sequence}"))?,
    );
    let doc_id = u64::from_be_bytes(
        entry[24..32]
            .try_into()
            .map_err(|_| format!("decode row-change id for entry {sequence}"))?,
    );
    let successor_doc_id = u64::from_be_bytes(
        entry[32..40]
            .try_into()
            .map_err(|_| format!("decode row-change successor for entry {sequence}"))?,
    );
    let successor_table_hash = u64::from_be_bytes(
        entry[40..48]
            .try_into()
            .map_err(|_| format!("decode row-change successor table for entry {sequence}"))?,
    );
    let kind = match entry[4] {
        1 => super::PublishedRowChangeKind::Update,
        2 => super::PublishedRowChangeKind::Delete,
        3 => super::PublishedRowChangeKind::Rewrite(super::PublishedRowIdentity {
            // Journals created before cross-partition successor tracking left these reserved bytes zeroed; such rewrites were necessarily within the source table.
            table_hash: if successor_table_hash == 0 {
                table_hash
            } else {
                successor_table_hash
            },
            doc_id: successor_doc_id,
        }),
        kind => {
            return Err(format!(
                "row-change journal entry {sequence} has invalid kind {kind}"
            ));
        }
    };
    Ok(super::PublishedRowChange {
        table_hash,
        doc_id,
        kind,
        strength: decode_strength(entry[5]).ok_or_else(|| {
            format!(
                "row-change journal entry {sequence} has invalid lock strength {}",
                entry[5]
            )
        })?,
    })
}

const fn strength_code(strength: LockStrength) -> u8 {
    match strength {
        LockStrength::ForKeyShare => 0,
        LockStrength::ForShare => 1,
        LockStrength::ForNoKeyUpdate => 2,
        LockStrength::ForUpdate => 3,
    }
}

const fn decode_strength(code: u8) -> Option<LockStrength> {
    match code {
        0 => Some(LockStrength::ForKeyShare),
        1 => Some(LockStrength::ForShare),
        2 => Some(LockStrength::ForNoKeyUpdate),
        3 => Some(LockStrength::ForUpdate),
        _ => None,
    }
}
