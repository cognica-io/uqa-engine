//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Cross-process row and relation lock coordination.
//!
//! Independent OS processes opening the same durable database coordinate
//! logical locks through native byte-range locks on a sidecar file
//! next to the database. Byte offsets derive from stable hashes of the
//! relation name and row identity; hash collisions only make coordination
//! more conservative, never less. Record locks die with the owning process,
//! so a crashed process can never leave a stale logical lock behind.
//!
//! Each row maps to a two-byte range whose first byte carries key-related
//! claims and whose second byte carries row-update claims. Mapping the four
//! `PostgreSQL` tuple-lock strengths onto shared and exclusive claims of
//! those two bytes reproduces the exact `PostgreSQL` 18 tuple-lock conflict
//! matrix across processes:
//!
//! - `FOR KEY SHARE`: shared claim of the key byte.
//! - `FOR SHARE`: shared claim of the row byte.
//! - `FOR NO KEY UPDATE`: exclusive claim of the row byte.
//! - `FOR UPDATE`: exclusive claims of both bytes.
//!
//! Fixed slot tables at the start of the sidecar record the exact session
//! holding or waiting for each byte. A waiter can therefore walk the
//! cross-process wait-for graph and report `40P01` only when it reaches its
//! own `(pid, session)`, mirroring `PostgreSQL`'s deadlock detector.

use uqa_sql::ast::LockStrength;

use super::{RelationLockMode, RowChangeTarget};

#[derive(Clone, Copy, Debug)]
pub(super) struct PublishedRowChange {
    pub table_hash: u64,
    pub doc_id: u64,
    pub kind: PublishedRowChangeKind,
    pub strength: LockStrength,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum PublishedRowChangeKind {
    Update,
    Delete,
    Rewrite(u64),
}

/// Sidecar layout. Coordination bytes and wait/holder slots occupy the low
/// addresses; record-lock byte ranges for relations and rows start above them
/// so lock offsets never alias structured data offsets.
const RELATION_BASE: u64 = 1 << 20;
const RELATION_SPAN: u64 = 1 << 20;
const ROW_BASE: u64 = 1 << 21;
const CHANGE_GATE_BYTE: u64 = 9;
/// Row byte pairs occupy `[ROW_BASE, ROW_BASE + 2 * ROW_SPAN)`. Record-lock
/// offsets travel through `off_t`, so the span is sized to the platform's
/// `off_t` width: 2^40 rows on 64-bit `off_t`, and the largest power of two
/// that keeps every offset below `i32::MAX` where `off_t` is 32 bits.
const ROW_SPAN: u64 = row_span_for_offset_width(std::mem::size_of::<OffsetWidth>());

#[cfg(unix)]
type OffsetWidth = libc::off_t;
#[cfg(not(unix))]
type OffsetWidth = i64;

const fn row_span_for_offset_width(bytes: usize) -> u64 {
    if bytes >= 8 {
        1 << 40
    } else {
        // (i32::MAX - ROW_BASE) / 2 rounded down to a power of two.
        1 << 29
    }
}

/// One advisory byte claim: `write` claims the byte exclusively.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ByteClaim {
    pub offset: u64,
    pub write: bool,
}

pub(super) const fn change_gate_claim(write: bool) -> ByteClaim {
    ByteClaim {
        offset: CHANGE_GATE_BYTE,
        write,
    }
}

pub(super) fn row_byte_claims(
    table: &str,
    doc_id: uqa_core::DocId,
    strength: LockStrength,
) -> Vec<ByteClaim> {
    let base = ROW_BASE + (stable_hash(&[table.as_bytes(), &doc_id.to_be_bytes()]) % ROW_SPAN) * 2;
    match strength {
        LockStrength::ForKeyShare => vec![ByteClaim {
            offset: base,
            write: false,
        }],
        LockStrength::ForShare => vec![ByteClaim {
            offset: base + 1,
            write: false,
        }],
        LockStrength::ForNoKeyUpdate => vec![ByteClaim {
            offset: base + 1,
            write: true,
        }],
        LockStrength::ForUpdate => vec![
            ByteClaim {
                offset: base,
                write: true,
            },
            ByteClaim {
                offset: base + 1,
                write: true,
            },
        ],
    }
}

pub(super) fn relation_byte_claims(table: &str, mode: RelationLockMode) -> Vec<ByteClaim> {
    let offset = RELATION_BASE + stable_hash(&[table.as_bytes()]) % RELATION_SPAN;
    vec![ByteClaim {
        offset,
        write: matches!(mode, RelationLockMode::AccessExclusive),
    }]
}

/// Stable identity of a relation name shared by every process.
pub(super) fn table_hash(table: &str) -> u64 {
    stable_hash(&[table.as_bytes()])
}

/// FNV-1a: the offsets must be identical in every process, so the hash key
/// cannot be process-random.
fn stable_hash(parts: &[&[u8]]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for part in parts {
        for byte in *part {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash ^= 0xff;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(any(unix, windows))]
pub(super) use file::FileLockCoordinator;

#[cfg(any(unix, windows))]
// Native record locks and process-liveness probes have no stable safe
// wrapper in std. The unsafe surface is confined to operating-system calls
// over file handles, process handles, and their plain C data structures.
#[allow(unsafe_code)]
mod file {
    use std::collections::HashMap;
    #[cfg(unix)]
    use std::os::fd::AsRawFd;
    #[cfg(unix)]
    use std::os::unix::fs::FileExt;
    #[cfg(windows)]
    use std::os::windows::fs::FileExt;
    #[cfg(windows)]
    use std::os::windows::io::AsRawHandle;
    use std::path::Path;

    use parking_lot::Mutex;
    use uqa_sql::ast::LockStrength;

    use super::{ByteClaim, RowChangeTarget};

    const CHANGE_JOURNAL_LOCK_BYTE: u64 = 10;
    const SLOT_METADATA_LOCK_BYTE: u64 = 11;
    #[cfg(windows)]
    const MODE_TRANSITION_LOCK_BYTE: u64 = 12;
    const WAIT_SLOT_BASE: u64 = 64;
    const WAIT_SLOT_SIZE: u64 = 32;
    const WAIT_SLOT_COUNT: u64 = 256;
    const HOLDER_SLOT_BASE: u64 = WAIT_SLOT_BASE + WAIT_SLOT_SIZE * WAIT_SLOT_COUNT;
    const HOLDER_SLOT_SIZE: u64 = 32;
    const HOLDER_SLOT_COUNT: u64 = 8192;
    const CHANGE_ENTRY_SIZE: u64 = 48;
    const CHANGE_ENTRY_MAGIC: u32 = 0x5551_4348;

    #[derive(Default)]
    struct ByteClaimCounts {
        shared: u64,
        exclusive: u64,
    }

    impl ByteClaimCounts {
        fn mode(&self) -> Option<bool> {
            if self.exclusive > 0 {
                Some(true)
            } else if self.shared > 0 {
                Some(false)
            } else {
                None
            }
        }
    }

    struct CoordinatorState {
        claims: HashMap<u64, ByteClaimCounts>,
        /// Sessions of this process holding each claimed byte, so a
        /// cross-process wait-for walk can attribute a locally held byte to
        /// the session that owns it and follow that session's own wait.
        holders: HashMap<u64, Vec<u64>>,
        /// Sidecar wait slot advertised for each locally waiting session.
        wait_slots: HashMap<u64, u64>,
        /// Sidecar holder slots for each acquisition owned by a local session.
        /// The vector preserves duplicate acquisitions of the same byte.
        holder_slots: HashMap<(u64, u64, bool), Vec<u64>>,
    }

    /// Process-wide coordinator for one durable database. All engine sessions
    /// of this process share one descriptor while the in-process lock table
    /// arbitrates between local sessions. On POSIX, nothing else in the
    /// process may open the sidecar path because closing another descriptor
    /// to it would drop this process's record locks.
    pub(in crate::row_locks) struct FileLockCoordinator {
        file: std::fs::File,
        change_file: std::fs::File,
        state: Mutex<CoordinatorState>,
    }

    impl FileLockCoordinator {
        pub(in crate::row_locks) fn open(database_path: &Path) -> Result<Self, String> {
            let mut sidecar = database_path.as_os_str().to_owned();
            sidecar.push(".uqa-locks");
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&sidecar)
                .map_err(|error| {
                    format!(
                        "open cross-process lock file `{}`: {error}",
                        Path::new(&sidecar).display()
                    )
                })?;
            let mut change_sidecar = database_path.as_os_str().to_owned();
            change_sidecar.push(".uqa-row-changes");
            let change_file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&change_sidecar)
                .map_err(|error| {
                    format!(
                        "open cross-process row-change journal `{}`: {error}",
                        Path::new(&change_sidecar).display()
                    )
                })?;
            let coordinator = Self {
                file,
                change_file,
                state: Mutex::new(CoordinatorState {
                    claims: HashMap::new(),
                    holders: HashMap::new(),
                    wait_slots: HashMap::new(),
                    holder_slots: HashMap::new(),
                }),
            };
            Ok(coordinator)
        }

        #[cfg(unix)]
        fn apply_byte_mode(
            &self,
            offset: u64,
            _before: Option<bool>,
            after: Option<bool>,
        ) -> std::io::Result<()> {
            let lock_type = match after {
                Some(true) => libc::F_WRLCK,
                Some(false) => libc::F_RDLCK,
                None => libc::F_UNLCK,
            };
            let mut flock: libc::flock = unsafe { std::mem::zeroed() };
            flock.l_type = lock_type as libc::c_short;
            flock.l_whence = libc::SEEK_SET as libc::c_short;
            flock.l_start = libc::off_t::try_from(offset).map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "record-lock offset exceeds the platform off_t range",
                )
            })?;
            flock.l_len = 1;
            let result = unsafe { libc::fcntl(self.file.as_raw_fd(), libc::F_SETLK, &flock) };
            if result == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        }

        #[cfg(windows)]
        fn apply_byte_mode(
            &self,
            offset: u64,
            before: Option<bool>,
            after: Option<bool>,
        ) -> std::io::Result<()> {
            if before == after {
                return Ok(());
            }
            while let Err(error) = windows_lock_byte(&self.file, MODE_TRANSITION_LOCK_BYTE, true) {
                if !lock_would_block(&error) {
                    return Err(error);
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            let transition = (|| {
                if before.is_some() {
                    windows_unlock_byte(&self.file, offset)?;
                }
                let result = match after {
                    Some(write) => windows_lock_byte(&self.file, offset, write),
                    None => Ok(()),
                };
                if result.is_err() {
                    if let Some(write) = before {
                        while windows_lock_byte(&self.file, offset, write).is_err() {
                            std::thread::sleep(std::time::Duration::from_millis(1));
                        }
                    }
                }
                result
            })();
            let unlock_transition = windows_unlock_byte(&self.file, MODE_TRANSITION_LOCK_BYTE);
            transition.and(unlock_transition)
        }

        fn holder_slot_offset(index: u64) -> u64 {
            HOLDER_SLOT_BASE + index * HOLDER_SLOT_SIZE
        }

        fn read_holder_slot(&self, index: u64) -> Option<HolderSlot> {
            let mut bytes = [0_u8; HOLDER_SLOT_SIZE as usize];
            read_exact_at(&self.file, &mut bytes, Self::holder_slot_offset(index)).ok()?;
            HolderSlot::decode(&bytes)
        }

        fn write_holder_slot(&self, index: u64, holder: Option<&HolderSlot>) {
            let bytes = holder.map_or([0_u8; HOLDER_SLOT_SIZE as usize], HolderSlot::encode);
            let _ = write_all_at(&self.file, &bytes, Self::holder_slot_offset(index));
        }

        fn acquire_slot_metadata_lock(&self) {
            while self
                .apply_byte_mode(SLOT_METADATA_LOCK_BYTE, None, Some(true))
                .is_err()
            {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }

        fn register_holder_slot(
            &self,
            state: &mut CoordinatorState,
            session: u64,
            claim: ByteClaim,
        ) {
            self.acquire_slot_metadata_lock();
            let pid = std::process::id();
            let preferred =
                stable_slot_hash(pid, session, claim.offset, claim.write) % HOLDER_SLOT_COUNT;
            let slot = (0..HOLDER_SLOT_COUNT).find_map(|probe| {
                let index = (preferred + probe) % HOLDER_SLOT_COUNT;
                let occupied = self.read_holder_slot(index).is_some_and(|existing| {
                    if existing.pid == pid {
                        state
                            .holder_slots
                            .values()
                            .any(|slots| slots.contains(&index))
                    } else {
                        process_alive(existing.pid)
                    }
                });
                (!occupied).then_some(index)
            });
            if let Some(index) = slot {
                self.write_holder_slot(
                    index,
                    Some(&HolderSlot {
                        pid,
                        session,
                        offset: claim.offset,
                        write: claim.write,
                    }),
                );
                state
                    .holder_slots
                    .entry((session, claim.offset, claim.write))
                    .or_default()
                    .push(index);
            }
            let _ = self.apply_byte_mode(SLOT_METADATA_LOCK_BYTE, Some(true), None);
        }

        fn clear_holder_slot(&self, state: &mut CoordinatorState, session: u64, claim: ByteClaim) {
            let key = (session, claim.offset, claim.write);
            let Some(slots) = state.holder_slots.get_mut(&key) else {
                return;
            };
            let Some(index) = slots.pop() else {
                return;
            };
            if slots.is_empty() {
                state.holder_slots.remove(&key);
            }
            self.acquire_slot_metadata_lock();
            self.write_holder_slot(index, None);
            let _ = self.apply_byte_mode(SLOT_METADATA_LOCK_BYTE, Some(true), None);
        }

        fn release_one(&self, state: &mut CoordinatorState, session: u64, claim: ByteClaim) {
            self.clear_holder_slot(state, session, claim);
            if let Some(holders) = state.holders.get_mut(&claim.offset) {
                if let Some(position) = holders.iter().position(|holder| *holder == session) {
                    holders.swap_remove(position);
                }
                if holders.is_empty() {
                    state.holders.remove(&claim.offset);
                }
            }
            let Some(counts) = state.claims.get_mut(&claim.offset) else {
                return;
            };
            let before = counts.mode();
            if claim.write {
                counts.exclusive = counts.exclusive.saturating_sub(1);
            } else {
                counts.shared = counts.shared.saturating_sub(1);
            }
            let after = counts.mode();
            if after.is_none() {
                state.claims.remove(&claim.offset);
            }
            if after != before {
                // Downgrading or unlocking a held range cannot block; an
                // I/O-level failure here would leave a stricter record lock
                // in place, which is conservative rather than unsound.
                let _ = self.apply_byte_mode(claim.offset, before, after);
            }
        }

        /// Try to add every claim without blocking. Either all claims are
        /// applied, or none are and the contended claim is reported.
        pub(in crate::row_locks) fn try_claim(
            &self,
            session: u64,
            claims: &[ByteClaim],
        ) -> Result<Result<(), ByteClaim>, String> {
            let mut state = self.state.lock();
            let mut applied: Vec<ByteClaim> = Vec::with_capacity(claims.len());
            for claim in claims {
                let counts = state.claims.entry(claim.offset).or_default();
                let before = counts.mode();
                if claim.write {
                    counts.exclusive += 1;
                } else {
                    counts.shared += 1;
                }
                let after = counts.mode();
                if after != before {
                    if let Err(error) = self.apply_byte_mode(claim.offset, before, after) {
                        // The record lock is unchanged; undo only the count.
                        let counts = state.claims.entry(claim.offset).or_default();
                        if claim.write {
                            counts.exclusive -= 1;
                        } else {
                            counts.shared -= 1;
                        }
                        if counts.mode().is_none() {
                            state.claims.remove(&claim.offset);
                        }
                        for undo in applied.iter().rev() {
                            self.release_one(&mut state, session, *undo);
                        }
                        if lock_would_block(&error) {
                            return Ok(Err(*claim));
                        }
                        return Err(format!("cross-process lock claim failed: {error}"));
                    }
                }
                state.holders.entry(claim.offset).or_default().push(session);
                self.register_holder_slot(&mut state, session, *claim);
                applied.push(*claim);
            }
            Ok(Ok(()))
        }

        /// Release claims that were successfully applied earlier by `session`.
        pub(in crate::row_locks) fn release(&self, session: u64, claims: &[ByteClaim]) {
            let mut state = self.state.lock();
            for claim in claims {
                self.release_one(&mut state, session, *claim);
            }
        }

        /// Append committed tuple-version events to an unbounded sidecar
        /// journal. Entries are never overwritten, so a long-lived statement
        /// cannot lose the generation history needed to distinguish an update
        /// chain from a delete followed by primary-key reuse.
        pub(in crate::row_locks) fn publish_changes(&self, changes: &[super::PublishedRowChange]) {
            if changes.is_empty() {
                return;
            }
            let _guard = self.state.lock();
            if self
                .apply_byte_mode(CHANGE_JOURNAL_LOCK_BYTE, None, Some(true))
                .is_err()
            {
                let mut attempts = 0;
                while self
                    .apply_byte_mode(CHANGE_JOURNAL_LOCK_BYTE, None, Some(true))
                    .is_err()
                {
                    attempts += 1;
                    if attempts > 200 {
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            }
            let Ok(mut next) = self.change_sequence_unlocked() else {
                let _ = self.apply_byte_mode(CHANGE_JOURNAL_LOCK_BYTE, Some(true), None);
                return;
            };
            for change in changes {
                let offset = next.saturating_mul(CHANGE_ENTRY_SIZE);
                let entry = encode_change_entry(next, change);
                if write_all_at(&self.change_file, &entry, offset).is_err() {
                    break;
                }
                next = next.wrapping_add(1);
            }
            let _ = self.change_file.sync_data();
            let _ = self.apply_byte_mode(CHANGE_JOURNAL_LOCK_BYTE, Some(true), None);
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
            let next = self.change_sequence()?;
            if baseline >= next {
                return Ok(RowChangeTarget::Unchanged);
            }
            let mut current = doc_id;
            let mut changed = false;
            for sequence in baseline..next {
                let offset = sequence.saturating_mul(CHANGE_ENTRY_SIZE);
                let mut entry = [0_u8; CHANGE_ENTRY_SIZE as usize];
                read_exact_at(&self.change_file, &mut entry, offset).map_err(|error| {
                    format!("read row-change journal entry {sequence} failed: {error}")
                })?;
                let event = decode_change_entry(sequence, &entry)?;
                if event.table_hash != table_hash || event.doc_id != current {
                    continue;
                }
                match event.kind {
                    super::PublishedRowChangeKind::Update => {
                        changed |= super::super::lock_strengths_conflict(event.strength, wanted);
                    }
                    super::PublishedRowChangeKind::Delete => {
                        if super::super::lock_strengths_conflict(event.strength, wanted) {
                            return Ok(RowChangeTarget::Deleted);
                        }
                    }
                    super::PublishedRowChangeKind::Rewrite(successor) => {
                        if super::super::lock_strengths_conflict(event.strength, wanted) {
                            current = successor;
                            changed = true;
                        }
                    }
                }
            }
            Ok(if changed {
                RowChangeTarget::Present(current)
            } else {
                RowChangeTarget::Unchanged
            })
        }

        fn slot_offset(index: u64) -> u64 {
            WAIT_SLOT_BASE + index * WAIT_SLOT_SIZE
        }

        fn read_slot(&self, index: u64) -> Option<WaitSlot> {
            let mut bytes = [0_u8; WAIT_SLOT_SIZE as usize];
            read_exact_at(&self.file, &mut bytes, Self::slot_offset(index)).ok()?;
            WaitSlot::decode(&bytes)
        }

        fn write_slot(&self, index: u64, slot: Option<&WaitSlot>) {
            let bytes = match slot {
                Some(slot) => slot.encode(),
                None => [0_u8; WAIT_SLOT_SIZE as usize],
            };
            let _ = write_all_at(&self.file, &bytes, Self::slot_offset(index));
        }

        /// Advertise what one session of this process is currently waiting
        /// for so other processes can walk the wait-for graph. Each waiting
        /// session owns its own slot; slot exhaustion degrades detection,
        /// never coordination.
        pub(in crate::row_locks) fn register_wait(&self, session: u64, claim: ByteClaim) {
            let pid = std::process::id();
            let slot = WaitSlot {
                pid,
                session,
                offset: claim.offset,
                write: claim.write,
            };
            let mut state = self.state.lock();
            self.acquire_slot_metadata_lock();
            if let Some(index) = state.wait_slots.get(&session).copied() {
                self.write_slot(index, Some(&slot));
                let _ = self.apply_byte_mode(SLOT_METADATA_LOCK_BYTE, Some(true), None);
                return;
            }
            let preferred =
                (u64::from(pid).wrapping_mul(31).wrapping_add(session)) % WAIT_SLOT_COUNT;
            for probe in 0..WAIT_SLOT_COUNT {
                let index = (preferred + probe) % WAIT_SLOT_COUNT;
                let occupied = self.read_slot(index).is_some_and(|existing| {
                    if existing.pid == pid {
                        // A slot of this process is live only while one of
                        // our sessions still owns it; stale slots from an
                        // earlier incarnation of this pid are reusable.
                        state.wait_slots.values().any(|used| *used == index)
                    } else {
                        process_alive(existing.pid)
                    }
                });
                if !occupied {
                    self.write_slot(index, Some(&slot));
                    state.wait_slots.insert(session, index);
                    break;
                }
            }
            let _ = self.apply_byte_mode(SLOT_METADATA_LOCK_BYTE, Some(true), None);
        }

        pub(in crate::row_locks) fn clear_wait(&self, session: u64) {
            let mut state = self.state.lock();
            if let Some(index) = state.wait_slots.remove(&session) {
                self.acquire_slot_metadata_lock();
                self.write_slot(index, None);
                let _ = self.apply_byte_mode(SLOT_METADATA_LOCK_BYTE, Some(true), None);
            }
        }

        /// Walk the cross-process wait-for graph from `wanted`, requested by
        /// local `session`. Foreign edges come from exact `(pid, session)`
        /// holder slots and the advertised wait of that same session. A byte
        /// held by this process is attributed to its local holder sessions:
        /// reaching the requesting session closes the cycle, an idle local
        /// holder ends that branch without a cycle, and a local holder that
        /// is itself waiting continues through `local_wait`, which reports
        /// the foreign byte a local session waits on, if any.
        pub(in crate::row_locks) fn wait_cycle_reaches_session(
            &self,
            session: u64,
            wanted: ByteClaim,
            local_wait: &dyn Fn(u64) -> Option<ByteClaim>,
        ) -> bool {
            let own_pid = std::process::id();
            let mut pending = vec![wanted];
            let mut seen_claims: Vec<ByteClaim> = Vec::new();
            while let Some(current) = pending.pop() {
                if seen_claims.contains(&current) {
                    continue;
                }
                seen_claims.push(current);
                for holder in self.local_holders_conflicting(current) {
                    if holder == session {
                        return true;
                    }
                    if let Some(next) = local_wait(holder) {
                        pending.push(next);
                    }
                }
                for holder in self.holder_sessions(current) {
                    if holder.pid == own_pid {
                        continue;
                    }
                    if let Some(wait) = self.wait_of(holder.pid, holder.session) {
                        pending.push(wait);
                    }
                }
            }
            false
        }

        /// Local sessions whose claims of `claim.offset` conflict with the
        /// requested claim.
        fn local_holders_conflicting(&self, claim: ByteClaim) -> Vec<u64> {
            let state = self.state.lock();
            let Some(counts) = state.claims.get(&claim.offset) else {
                return Vec::new();
            };
            let conflicts = counts.exclusive > 0 || (claim.write && counts.shared > 0);
            if !conflicts {
                return Vec::new();
            }
            state
                .holders
                .get(&claim.offset)
                .cloned()
                .unwrap_or_default()
        }

        fn holder_sessions(&self, claim: ByteClaim) -> Vec<HolderSlot> {
            let mut holders = Vec::new();
            for index in 0..HOLDER_SLOT_COUNT {
                if let Some(holder) = self.read_holder_slot(index) {
                    if holder.offset == claim.offset
                        && (holder.write || claim.write)
                        && process_alive(holder.pid)
                    {
                        holders.push(holder);
                    }
                }
            }
            holders
        }

        fn wait_of(&self, pid: u32, session: u64) -> Option<ByteClaim> {
            for index in 0..WAIT_SLOT_COUNT {
                if let Some(slot) = self.read_slot(index) {
                    if slot.pid == pid && slot.session == session {
                        return Some(ByteClaim {
                            offset: slot.offset,
                            write: slot.write,
                        });
                    }
                }
            }
            None
        }
    }

    #[cfg(unix)]
    fn read_exact_at(file: &std::fs::File, bytes: &mut [u8], offset: u64) -> std::io::Result<()> {
        file.read_exact_at(bytes, offset)
    }

    #[cfg(unix)]
    fn write_all_at(file: &std::fs::File, bytes: &[u8], offset: u64) -> std::io::Result<()> {
        file.write_all_at(bytes, offset)
    }

    #[cfg(windows)]
    fn read_exact_at(file: &std::fs::File, bytes: &mut [u8], offset: u64) -> std::io::Result<()> {
        let mut consumed = 0usize;
        while consumed < bytes.len() {
            let read = file.seek_read(
                &mut bytes[consumed..],
                offset.saturating_add(consumed as u64),
            )?;
            if read == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "positioned file read reached end of file",
                ));
            }
            consumed += read;
        }
        Ok(())
    }

    #[cfg(windows)]
    fn write_all_at(file: &std::fs::File, bytes: &[u8], offset: u64) -> std::io::Result<()> {
        let mut consumed = 0usize;
        while consumed < bytes.len() {
            let written =
                file.seek_write(&bytes[consumed..], offset.saturating_add(consumed as u64))?;
            if written == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    "positioned file write returned zero bytes",
                ));
            }
            consumed += written;
        }
        Ok(())
    }

    #[cfg(unix)]
    fn lock_would_block(error: &std::io::Error) -> bool {
        matches!(error.raw_os_error(), Some(libc::EAGAIN | libc::EACCES))
    }

    #[cfg(windows)]
    fn lock_would_block(error: &std::io::Error) -> bool {
        error.raw_os_error() == Some(windows_sys::Win32::Foundation::ERROR_LOCK_VIOLATION as i32)
    }

    #[cfg(windows)]
    fn windows_overlapped(offset: u64) -> windows_sys::Win32::System::IO::OVERLAPPED {
        let mut overlapped = windows_sys::Win32::System::IO::OVERLAPPED::default();
        overlapped.Anonymous = windows_sys::Win32::System::IO::OVERLAPPED_0 {
            Anonymous: windows_sys::Win32::System::IO::OVERLAPPED_0_0 {
                Offset: offset as u32,
                OffsetHigh: (offset >> 32) as u32,
            },
        };
        overlapped
    }

    #[cfg(windows)]
    fn windows_lock_byte(file: &std::fs::File, offset: u64, write: bool) -> std::io::Result<()> {
        use windows_sys::Win32::Storage::FileSystem::{
            LockFileEx, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY,
        };
        let mut overlapped = windows_overlapped(offset);
        let flags = LOCKFILE_FAIL_IMMEDIATELY | if write { LOCKFILE_EXCLUSIVE_LOCK } else { 0 };
        let result =
            unsafe { LockFileEx(file.as_raw_handle(), flags, 0, 1, 0, &raw mut overlapped) };
        if result == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    #[cfg(windows)]
    fn windows_unlock_byte(file: &std::fs::File, offset: u64) -> std::io::Result<()> {
        let mut overlapped = windows_overlapped(offset);
        let result = unsafe {
            windows_sys::Win32::Storage::FileSystem::UnlockFileEx(
                file.as_raw_handle(),
                0,
                1,
                0,
                &raw mut overlapped,
            )
        };
        if result == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    fn encode_change_entry(
        sequence: u64,
        change: &super::PublishedRowChange,
    ) -> [u8; CHANGE_ENTRY_SIZE as usize] {
        let mut entry = [0_u8; CHANGE_ENTRY_SIZE as usize];
        entry[0..4].copy_from_slice(&CHANGE_ENTRY_MAGIC.to_be_bytes());
        let (kind, successor) = match change.kind {
            super::PublishedRowChangeKind::Update => (1, 0),
            super::PublishedRowChangeKind::Delete => (2, 0),
            super::PublishedRowChangeKind::Rewrite(successor) => (3, successor),
        };
        entry[4] = kind;
        entry[5] = strength_code(change.strength);
        entry[8..16].copy_from_slice(&sequence.wrapping_add(1).to_be_bytes());
        entry[16..24].copy_from_slice(&change.table_hash.to_be_bytes());
        entry[24..32].copy_from_slice(&change.doc_id.to_be_bytes());
        entry[32..40].copy_from_slice(&successor.to_be_bytes());
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
        let successor = u64::from_be_bytes(
            entry[32..40]
                .try_into()
                .map_err(|_| format!("decode row-change successor for entry {sequence}"))?,
        );
        let kind = match entry[4] {
            1 => super::PublishedRowChangeKind::Update,
            2 => super::PublishedRowChangeKind::Delete,
            3 => super::PublishedRowChangeKind::Rewrite(successor),
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

    fn stable_slot_hash(pid: u32, session: u64, offset: u64, write: bool) -> u64 {
        u64::from(pid)
            .wrapping_mul(0x9e37_79b9_7f4a_7c15)
            .wrapping_add(session.rotate_left(17))
            .wrapping_add(offset.rotate_left(31))
            .wrapping_add(u64::from(write))
    }

    #[cfg(unix)]
    fn process_alive(pid: u32) -> bool {
        if unsafe { libc::kill(pid as libc::pid_t, 0) } == 0 {
            return true;
        }
        std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    }

    #[cfg(windows)]
    fn process_alive(pid: u32) -> bool {
        use windows_sys::Win32::Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, STILL_ACTIVE};
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle.is_null() {
            return std::io::Error::last_os_error().raw_os_error()
                != Some(ERROR_INVALID_PARAMETER as i32);
        }
        let mut exit_code = 0u32;
        let queried = unsafe { GetExitCodeProcess(handle, &raw mut exit_code) } != 0;
        let _ = unsafe { CloseHandle(handle) };
        !queried || exit_code == STILL_ACTIVE as u32
    }

    struct WaitSlot {
        pid: u32,
        session: u64,
        offset: u64,
        write: bool,
    }

    struct HolderSlot {
        pid: u32,
        session: u64,
        offset: u64,
        write: bool,
    }

    impl HolderSlot {
        const MAGIC: u32 = 0x5551_484c;

        fn encode(&self) -> [u8; HOLDER_SLOT_SIZE as usize] {
            let mut bytes = [0_u8; HOLDER_SLOT_SIZE as usize];
            bytes[0..4].copy_from_slice(&Self::MAGIC.to_be_bytes());
            bytes[4..8].copy_from_slice(&self.pid.to_be_bytes());
            bytes[8..16].copy_from_slice(&self.offset.to_be_bytes());
            bytes[16] = u8::from(self.write);
            bytes[24..32].copy_from_slice(&self.session.to_be_bytes());
            bytes
        }

        fn decode(bytes: &[u8; HOLDER_SLOT_SIZE as usize]) -> Option<Self> {
            if bytes[0..4] != Self::MAGIC.to_be_bytes() {
                return None;
            }
            let pid = u32::from_be_bytes(bytes[4..8].try_into().ok()?);
            if pid == 0 {
                return None;
            }
            Some(Self {
                pid,
                session: u64::from_be_bytes(bytes[24..32].try_into().ok()?),
                offset: u64::from_be_bytes(bytes[8..16].try_into().ok()?),
                write: bytes[16] != 0,
            })
        }
    }

    impl WaitSlot {
        const MAGIC: u32 = 0x5551_4c4b;

        fn encode(&self) -> [u8; WAIT_SLOT_SIZE as usize] {
            let mut bytes = [0_u8; WAIT_SLOT_SIZE as usize];
            bytes[0..4].copy_from_slice(&Self::MAGIC.to_be_bytes());
            bytes[4..8].copy_from_slice(&self.pid.to_be_bytes());
            bytes[8..16].copy_from_slice(&self.offset.to_be_bytes());
            bytes[16] = u8::from(self.write);
            bytes[24..32].copy_from_slice(&self.session.to_be_bytes());
            bytes
        }

        fn decode(bytes: &[u8; WAIT_SLOT_SIZE as usize]) -> Option<Self> {
            if bytes[0..4] != Self::MAGIC.to_be_bytes() {
                return None;
            }
            let pid = u32::from_be_bytes(bytes[4..8].try_into().ok()?);
            if pid == 0 {
                return None;
            }
            Some(Self {
                pid,
                session: u64::from_be_bytes(bytes[24..32].try_into().ok()?),
                offset: u64::from_be_bytes(bytes[8..16].try_into().ok()?),
                write: bytes[16] != 0,
            })
        }
    }
}

#[cfg(not(any(unix, windows)))]
pub(super) use fallback::FileLockCoordinator;

#[cfg(not(any(unix, windows)))]
mod fallback {
    use std::path::Path;

    use super::ByteClaim;

    /// Sandboxed targets without native processes retain process-local lock
    /// semantics instead of rejecting every persistent mutation.
    pub(in crate::row_locks) struct FileLockCoordinator {}

    impl FileLockCoordinator {
        pub(in crate::row_locks) fn open(_database_path: &Path) -> Result<Self, String> {
            Ok(Self {})
        }

        pub(in crate::row_locks) fn try_claim(
            &self,
            _session: u64,
            _claims: &[ByteClaim],
        ) -> Result<Result<(), ByteClaim>, String> {
            Ok(Ok(()))
        }

        pub(in crate::row_locks) fn release(&self, _session: u64, _claims: &[ByteClaim]) {}

        pub(in crate::row_locks) fn register_wait(&self, _session: u64, _claim: ByteClaim) {}

        pub(in crate::row_locks) fn clear_wait(&self, _session: u64) {}

        pub(in crate::row_locks) fn wait_cycle_reaches_session(
            &self,
            _session: u64,
            _wanted: ByteClaim,
            _local_wait: &dyn Fn(u64) -> Option<ByteClaim>,
        ) -> bool {
            false
        }

        pub(in crate::row_locks) fn publish_changes(&self, _changes: &[super::PublishedRowChange]) {
        }

        pub(in crate::row_locks) fn change_sequence(&self) -> Result<u64, String> {
            Ok(0)
        }

        pub(in crate::row_locks) fn change_target_after(
            &self,
            _table_hash: u64,
            _doc_id: u64,
            _baseline: u64,
            _wanted: uqa_sql::ast::LockStrength,
        ) -> Result<super::RowChangeTarget, String> {
            Ok(super::RowChangeTarget::Unchanged)
        }
    }
}
