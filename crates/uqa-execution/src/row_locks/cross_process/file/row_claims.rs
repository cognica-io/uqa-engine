//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact row claims shared by every process attached to one database.
//!
//! A record lock for each claimed row byte made a statement's lock cost grow with the square of its row count, because each record lock call walks every record lock of the file. Row claims are therefore entries of a table in their own sidecar, as `PostgreSQL` names a tuple's lockers in the tuple and makes a waiter wait for the locking transaction instead of holding one lock manager entry per tuple.
//!
//! An entry holds the modes one session of one process claims on the two bytes of a row. Sessions of one process arbitrate in the in-process lock table, so a claim conflicts only with entries of other processes. Each attached process holds one record lock, its liveness byte, until it exits; an entry whose owner no longer holds that byte is removed by whoever meets it.

use std::collections::{HashMap, HashSet};

use uqa_storage::native_file::{lock_byte, try_lock_byte, unlock_byte};

use super::super::{row_claim, row_claim_address, RowByte, PROCESS_LIVENESS_BASE};
use super::waits::HolderSlot;
use super::{lock_would_block, ByteClaim, CoordinatorState, FileLockCoordinator};

mod table;
use table::{Entry, Header, Mode, Owner, Slot, Table};

#[cfg(test)]
mod tests;

const TABLE_LOCK_BYTE: u64 = 7;
/// A run longer than this is rebuilt, which removes its tombstones and the claims of dead processes.
const PROBE_LIMIT: u64 = 64;

/// How many times one session claimed each byte of a row in each mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Counts {
    key_shared: u32,
    key_exclusive: u32,
    row_shared: u32,
    row_exclusive: u32,
}

impl Counts {
    fn count(&mut self, byte: RowByte, write: bool) -> &mut u32 {
        match (byte, write) {
            (RowByte::Key, false) => &mut self.key_shared,
            (RowByte::Key, true) => &mut self.key_exclusive,
            (RowByte::Row, false) => &mut self.row_shared,
            (RowByte::Row, true) => &mut self.row_exclusive,
        }
    }

    fn mode(&self, byte: RowByte) -> Mode {
        let (shared, exclusive) = match byte {
            RowByte::Key => (self.key_shared, self.key_exclusive),
            RowByte::Row => (self.row_shared, self.row_exclusive),
        };
        if exclusive > 0 {
            Mode::Exclusive
        } else if shared > 0 {
            Mode::Shared
        } else {
            Mode::None
        }
    }

    fn modes(&self) -> (Mode, Mode) {
        (self.mode(RowByte::Key), self.mode(RowByte::Row))
    }
}

fn entry_mode(entry: &Entry, byte: RowByte) -> Mode {
    match byte {
        RowByte::Key => entry.key,
        RowByte::Row => entry.row,
    }
}

/// Whether a holder in `held` blocks a claim in `wanted` of the same byte.
fn modes_conflict(wanted: Mode, held: Mode) -> bool {
    wanted != Mode::None
        && held != Mode::None
        && (wanted == Mode::Exclusive || held == Mode::Exclusive)
}

/// The claims of one row by the sessions of this process.
struct SessionClaim {
    session: u64,
    counts: Counts,
}

/// One session's claims of one row before and after a claim or release.
struct Change {
    identity: u64,
    before: Counts,
    after: Counts,
}

impl Change {
    fn changes_entry(&self) -> bool {
        self.before.modes() != self.after.modes()
    }
}

/// Where a session's entry for a row is, or where it would go.
#[derive(Default)]
struct Placement {
    own: Option<u64>,
    /// The first tombstone of the run, which a new entry reuses.
    tombstone: Option<u64>,
    empty: Option<u64>,
    length: u64,
}

/// This process's row claims and its attachment to the claim table.
#[derive(Default)]
pub(super) struct RowClaims {
    owner: Option<Owner>,
    identities: HashMap<u64, Vec<SessionClaim>>,
    /// Entries of this process in the claim table.
    entries: u64,
    /// Owners found dead. An owner is never named twice, so a dead owner stays dead.
    dead: HashSet<Owner>,
}

impl RowClaims {
    fn counts(&self, session: u64, identity: u64) -> Counts {
        self.identities
            .get(&identity)
            .and_then(|claims| claims.iter().find(|claim| claim.session == session))
            .map(|claim| claim.counts)
            .unwrap_or_default()
    }

    fn set_counts(&mut self, session: u64, identity: u64, counts: Counts) {
        if counts == Counts::default() {
            if let Some(claims) = self.identities.get_mut(&identity) {
                claims.retain(|claim| claim.session != session);
                if claims.is_empty() {
                    self.identities.remove(&identity);
                }
            }
            return;
        }
        let claims = self.identities.entry(identity).or_default();
        match claims.iter_mut().find(|claim| claim.session == session) {
            Some(claim) => claim.counts = counts,
            None => claims.push(SessionClaim { session, counts }),
        }
    }

    /// Local sessions whose claims block `claim`.
    pub(super) fn holders(&self, claim: ByteClaim) -> Vec<u64> {
        let Some((identity, byte)) = row_claim_address(claim) else {
            return Vec::new();
        };
        let wanted = if claim.write {
            Mode::Exclusive
        } else {
            Mode::Shared
        };
        self.identities
            .get(&identity)
            .into_iter()
            .flatten()
            .filter(|held| modes_conflict(wanted, held.counts.mode(byte)))
            .map(|held| held.session)
            .collect()
    }
}

/// The claim table lock byte, held until dropped.
struct TableLockByte<'a>(&'a FileLockCoordinator);

impl Drop for TableLockByte<'_> {
    fn drop(&mut self) {
        let _ = unlock_byte(&self.0.file, TABLE_LOCK_BYTE);
    }
}

/// The claim table for one operation: its lock byte and the header read under it.
struct TableLock<'a> {
    byte: TableLockByte<'a>,
    header: Header,
}

impl TableLock<'_> {
    fn table(&self) -> Table<'_> {
        Table::new(&self.byte.0.claim_file, &self.header)
    }
}

fn liveness_byte(slot: u16) -> u64 {
    PROCESS_LIVENESS_BASE + u64::from(slot)
}

fn table_error(action: &str, error: &std::io::Error) -> String {
    format!("{action} the cross-process row claim table: {error}")
}

/// Group the claims of one session by row. `claims` ascend by address, which keeps the claims of one row together.
fn changes(
    rows: &RowClaims,
    session: u64,
    claims: &[ByteClaim],
    apply: impl Fn(&mut u32),
) -> Vec<Change> {
    let mut changes: Vec<Change> = Vec::with_capacity(1);
    for claim in claims {
        let Some((identity, byte)) = row_claim_address(*claim) else {
            continue;
        };
        if changes.last().is_none_or(|last| last.identity != identity) {
            let before = rows.counts(session, identity);
            changes.push(Change {
                identity,
                before,
                after: before,
            });
        }
        let change = changes.last_mut().expect("a change for the row");
        apply(change.after.count(byte, claim.write));
    }
    changes
}

impl FileLockCoordinator {
    /// Take the claim table lock byte, attach this process on its first use and finish a rebuild a dead process left behind.
    fn lock_row_claim_table(&self, rows: &mut RowClaims) -> Result<TableLock<'_>, String> {
        lock_byte(&self.file, TABLE_LOCK_BYTE, true)
            .map_err(|error| table_error("lock", &error))?;
        let byte = TableLockByte(self);
        let stored = table::read_header(&self.claim_file);
        if rows.owner.is_none() && self.attach_row_claims(rows)? {
            // No other process is attached, so nothing in the table is claimed.
            let header =
                table::initialize(&self.claim_file, stored.ok().flatten()).map_err(|error| {
                    // Without a table this process is not attached; its next claim attaches again.
                    if let Some(owner) = rows.owner.take() {
                        let _ = unlock_byte(&self.file, liveness_byte(owner.slot));
                    }
                    table_error("initialize", &error)
                })?;
            return Ok(TableLock { byte, header });
        }
        let mut header = stored
            .map_err(|error| table_error("read", &error))?
            .ok_or_else(|| "the cross-process row claim table has no header".to_string())?;
        table::recover(&self.claim_file, &mut header)
            .map_err(|error| table_error("recover", &error))?;
        Ok(TableLock { byte, header })
    }

    /// Attach this process to the claim table if it is not attached yet.
    pub(super) fn attach_row_claims_process(&self) -> Result<(), String> {
        let mut state = self.state.lock();
        if state.rows.owner.is_none() {
            drop(self.lock_row_claim_table(&mut state.rows)?);
        }
        Ok(())
    }

    /// Before the liveness byte of an attached process is released: when it is the last one attached, mark the sequence positions clean.
    pub(super) fn detach_row_claims_process(&mut self) {
        let Some(own) = self.state.get_mut().rows.owner else {
            return;
        };
        if lock_byte(&self.file, TABLE_LOCK_BYTE, true).is_err() {
            return;
        }
        let byte = TableLockByte(self);
        if byte
            .0
            .other_row_claim_processes(own)
            .is_ok_and(|others| !others)
        {
            let _ = byte.0.mark_sequence_positions_clean();
        }
    }

    /// Whether a process other than `own` holds a liveness byte. The caller holds the claim table lock byte.
    fn other_row_claim_processes(&self, own: Owner) -> std::io::Result<bool> {
        for (slot, (generation, _)) in (0_u16..).zip(table::read_processes(&self.claim_file)?) {
            if generation == 0 {
                break;
            }
            if slot == own.slot {
                continue;
            }
            match try_lock_byte(&self.file, liveness_byte(slot), true) {
                Ok(()) => unlock_byte(&self.file, liveness_byte(slot))?,
                Err(error) if lock_would_block(&error) => return Ok(true),
                Err(error) => return Err(error),
            }
        }
        Ok(false)
    }

    /// Take the first process slot no live process holds and return whether this process is the only one attached. The caller holds the claim table lock byte, which orders attachments.
    fn attach_row_claims(&self, rows: &mut RowClaims) -> Result<bool, String> {
        let mut alone = true;
        let mut owner = None;
        let attached = (|| {
            for (slot, (generation, _)) in (0_u16..).zip(table::read_processes(&self.claim_file)?) {
                // Slots are taken in order, so no slot after a never-used one was ever used.
                if owner.is_some() && generation == 0 {
                    break;
                }
                match try_lock_byte(&self.file, liveness_byte(slot), true) {
                    Ok(()) if owner.is_none() => {
                        let generation = generation.wrapping_add(1).max(1);
                        owner = Some(Owner { slot, generation });
                        table::write_process(
                            &self.claim_file,
                            slot,
                            generation,
                            std::process::id(),
                        )?;
                    }
                    Ok(()) => unlock_byte(&self.file, liveness_byte(slot))?,
                    Err(error) if lock_would_block(&error) => alone = false,
                    Err(error) => return Err(error),
                }
            }
            Ok(())
        })();
        match (attached, owner) {
            (Ok(()), Some(owner)) => {
                if alone {
                    // No process remains from before, so a machine failure may have left older sequence positions behind.
                    if let Err(error) = self.begin_sequence_position_run() {
                        let _ = unlock_byte(&self.file, liveness_byte(owner.slot));
                        return Err(table_error("attach to", &error));
                    }
                    rows.dead.clear();
                }
                rows.owner = Some(owner);
                Ok(alone)
            }
            (Ok(()), None) => {
                Err("every process slot of the cross-process row claim table is in use".to_string())
            }
            (Err(error), owner) => {
                // A liveness byte this process keeps without being attached would name it a live owner of nothing.
                if let Some(owner) = owner {
                    let _ = unlock_byte(&self.file, liveness_byte(owner.slot));
                }
                Err(table_error("attach to", &error))
            }
        }
    }

    /// Whether the process that wrote an entry still holds its liveness byte. The caller holds the claim table lock byte.
    fn row_claim_owner_alive(&self, rows: &mut RowClaims, owner: Owner) -> std::io::Result<bool> {
        let own = rows.owner.expect("attached to the row claim table");
        if owner.slot == own.slot {
            // This process holds the byte of its own slot; an earlier attachment of the slot is dead.
            return Ok(owner == own);
        }
        if rows.dead.contains(&owner) {
            return Ok(false);
        }
        let (generation, _) = table::read_process(&self.claim_file, owner.slot)?;
        let alive = generation == owner.generation
            && match try_lock_byte(&self.file, liveness_byte(owner.slot), true) {
                Ok(()) => {
                    unlock_byte(&self.file, liveness_byte(owner.slot))?;
                    false
                }
                Err(error) if lock_would_block(&error) => true,
                Err(error) => return Err(error),
            };
        if !alive {
            rows.dead.insert(owner);
        }
        Ok(alive)
    }

    /// Find `session`'s entry for a row and the first byte of `change` a live entry of another process blocks. Entries of dead processes met on the way are removed.
    fn place_row_claim(
        &self,
        rows: &mut RowClaims,
        lock: &TableLock<'_>,
        session: u64,
        change: &Change,
    ) -> std::io::Result<(Placement, Option<RowByte>)> {
        let own = rows.owner.expect("attached to the row claim table");
        let table = lock.table();
        let mut placement = Placement::default();
        let mut blocked = None;
        let mut dead = Vec::new();
        let probe = table.probe(change.identity, &mut |index, slot| {
            let entry = match slot {
                Slot::Live(entry) if entry.identity == change.identity => entry,
                Slot::Tombstone => {
                    placement.tombstone.get_or_insert(index);
                    return Ok(());
                }
                _ => return Ok(()),
            };
            if entry.owner == own {
                if entry.session == session {
                    placement.own = Some(index);
                }
                return Ok(());
            }
            let blocking = [RowByte::Key, RowByte::Row].into_iter().find(|byte| {
                let wanted = change.after.mode(*byte);
                wanted > change.before.mode(*byte)
                    && modes_conflict(wanted, entry_mode(&entry, *byte))
            });
            // Only an entry that would block is worth the liveness probe; a rebuild removes the others.
            if let Some(byte) = blocking {
                if self.row_claim_owner_alive(rows, entry.owner)? {
                    blocked.get_or_insert(byte);
                } else {
                    dead.push(index);
                }
            }
            Ok(())
        })?;
        placement.empty = probe.empty;
        placement.length = probe.length;
        for index in dead {
            table.write(index, &Slot::Tombstone)?;
            placement.tombstone = Some(placement.tombstone.map_or(index, |first| first.min(index)));
        }
        Ok((placement, blocked))
    }

    /// Replace the table with one holding the claims of live processes and room for `additional` more.
    fn rebuild_row_claims(
        &self,
        rows: &mut RowClaims,
        lock: &mut TableLock<'_>,
        additional: u64,
    ) -> std::io::Result<()> {
        let mut entries = Vec::new();
        // One probe answers for every claim of a process. A process that dies after it leaves claims that are removed like any dead process's.
        let mut alive = HashSet::new();
        lock.table().scan(&mut |entry| {
            if alive.contains(&entry.owner) || self.row_claim_owner_alive(rows, entry.owner)? {
                alive.insert(entry.owner);
                entries.push(entry);
            }
            Ok(())
        })?;
        table::rebuild(&self.claim_file, &mut lock.header, entries, additional)
    }

    /// Write `session`'s entry for a row after `change`.
    fn store_row_claim(
        &self,
        rows: &mut RowClaims,
        lock: &mut TableLock<'_>,
        session: u64,
        change: &Change,
        mut placement: Placement,
    ) -> std::io::Result<()> {
        let own = rows.owner.expect("attached to the row claim table");
        let (key, row) = change.after.modes();
        let entry = Slot::Live(Entry {
            identity: change.identity,
            session,
            owner: own,
            key,
            row,
        });
        if let Some(index) = placement.own {
            return lock.table().write(index, &entry);
        }
        // A table more than half full of this process's claims grows before its runs lengthen.
        if (rows.entries + 1) * 2 > lock.header.capacity()
            || (placement.tombstone.is_none() && placement.empty.is_none())
        {
            self.rebuild_row_claims(rows, lock, 1)?;
            placement = self.place_row_claim(rows, lock, session, change)?.0;
        }
        let index = placement
            .tombstone
            .or(placement.empty)
            .ok_or_else(|| std::io::Error::other("a rebuilt row claim table has no free slot"))?;
        lock.table().write(index, &entry)?;
        rows.entries += 1;
        if placement.length > PROBE_LIMIT {
            self.rebuild_row_claims(rows, lock, 0)?;
        }
        Ok(())
    }

    /// Add `claims`, all of rows, for `session`. Either every claim is added, or none is and a claim that a live entry of another process blocks is reported.
    pub(super) fn try_claim_rows(
        &self,
        state: &mut CoordinatorState,
        session: u64,
        claims: &[ByteClaim],
    ) -> Result<Result<(), ByteClaim>, String> {
        let rows = &mut state.rows;
        let mut ordered = std::borrow::Cow::Borrowed(claims);
        if !claims.is_sorted_by_key(|claim| claim.offset) {
            ordered.to_mut().sort_unstable_by_key(|claim| claim.offset);
        }
        let changes = changes(rows, session, &ordered, |count| *count += 1);
        let stored = changes
            .iter()
            .filter(|change| change.changes_entry())
            .collect::<Vec<_>>();
        if !stored.is_empty() {
            let mut lock = self.lock_row_claim_table(rows)?;
            // Every conflict is found before any entry is written, so a blocked claim leaves the table unchanged.
            let mut placements = Vec::with_capacity(stored.len());
            for change in &stored {
                let (placement, blocked) = self
                    .place_row_claim(rows, &lock, session, change)
                    .map_err(|error| table_error("read", &error))?;
                if let Some(byte) = blocked {
                    let write = change.after.mode(byte) == Mode::Exclusive;
                    return Ok(Err(row_claim(change.identity, byte, write)));
                }
                placements.push(placement);
            }
            // Only the placement of a single row is still exact once an entry has been written.
            let single = placements.len() == 1;
            for (written, (change, placement)) in stored.iter().zip(placements).enumerate() {
                let result = if single {
                    Ok(placement)
                } else {
                    self.place_row_claim(rows, &lock, session, change)
                        .map(|(placement, _)| placement)
                }
                .and_then(|placement| {
                    self.store_row_claim(rows, &mut lock, session, change, placement)
                });
                if let Err(error) = result {
                    // The entries already written would claim rows this session was never granted.
                    for change in &stored[..written] {
                        let restored = Change {
                            identity: change.identity,
                            before: change.after,
                            after: change.before,
                        };
                        let _ = self.release_row_claim(rows, &lock, session, &restored);
                    }
                    return Err(table_error("write", &error));
                }
            }
        }
        for change in changes {
            rows.set_counts(session, change.identity, change.after);
        }
        Ok(Ok(()))
    }

    /// Drop `claims`, all of rows and ascending by address, that `session` holds. A table that cannot be updated keeps the entry, which is conservative while this process lives.
    pub(super) fn release_rows(
        &self,
        state: &mut CoordinatorState,
        session: u64,
        claims: &[ByteClaim],
    ) {
        if claims.is_empty() {
            return;
        }
        let rows = &mut state.rows;
        let changes = changes(rows, session, claims, |count| {
            *count = count.saturating_sub(1);
        });
        if changes.iter().any(Change::changes_entry) {
            if let Ok(mut lock) = self.lock_row_claim_table(rows) {
                for change in changes.iter().filter(|change| change.changes_entry()) {
                    let _ = self.release_row_claim(rows, &lock, session, change);
                }
                if rows.entries == 0 {
                    let _ = self.shrink_row_claims(rows, &mut lock);
                }
            }
        }
        for change in changes {
            rows.set_counts(session, change.identity, change.after);
        }
    }

    fn release_row_claim(
        &self,
        rows: &mut RowClaims,
        lock: &TableLock<'_>,
        session: u64,
        change: &Change,
    ) -> std::io::Result<()> {
        let (placement, _) = self.place_row_claim(rows, lock, session, change)?;
        let Some(index) = placement.own else {
            return Ok(());
        };
        let (key, row) = change.after.modes();
        if key == Mode::None && row == Mode::None {
            lock.table().remove(index)?;
            rows.entries = rows.entries.saturating_sub(1);
            return Ok(());
        }
        lock.table().write(
            index,
            &Slot::Live(Entry {
                identity: change.identity,
                session,
                owner: rows.owner.expect("attached to the row claim table"),
                key,
                row,
            }),
        )
    }

    /// Return a grown table that holds no claim of this process to its initial capacity when no other process is attached.
    fn shrink_row_claims(
        &self,
        rows: &mut RowClaims,
        lock: &mut TableLock<'_>,
    ) -> std::io::Result<()> {
        if lock.header.capacity_log2 == table::INITIAL_CAPACITY_LOG2 {
            return Ok(());
        }
        let own = rows.owner.expect("attached to the row claim table");
        if self.other_row_claim_processes(own)? {
            return Ok(());
        }
        lock.header = table::initialize(&self.claim_file, Some(lock.header))?;
        rows.dead.clear();
        Ok(())
    }

    /// Sessions of other live processes whose claims block `claim`.
    pub(super) fn foreign_row_holders(&self, claim: ByteClaim) -> Vec<HolderSlot> {
        let Some((identity, byte)) = row_claim_address(claim) else {
            return Vec::new();
        };
        let wanted = if claim.write {
            Mode::Exclusive
        } else {
            Mode::Shared
        };
        let mut state = self.state.lock();
        let rows = &mut state.rows;
        let Ok(lock) = self.lock_row_claim_table(rows) else {
            return Vec::new();
        };
        let own = rows.owner.expect("attached to the row claim table");
        let mut holders = Vec::new();
        let _ = lock.table().probe(identity, &mut |_, slot| {
            let Slot::Live(entry) = slot else {
                return Ok(());
            };
            if entry.identity != identity
                || entry.owner == own
                || !modes_conflict(wanted, entry_mode(&entry, byte))
                || !self.row_claim_owner_alive(rows, entry.owner)?
            {
                return Ok(());
            }
            let (_, pid) = table::read_process(&self.claim_file, entry.owner.slot)?;
            holders.push(HolderSlot {
                pid,
                session: entry.session,
                offset: claim.offset,
                write: entry_mode(&entry, byte) == Mode::Exclusive,
            });
            Ok(())
        });
        holders
    }
}
