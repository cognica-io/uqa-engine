//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Holder and waiter slots plus cross-process wait-graph traversal.

use super::super::{row_claim_address, wait_blocking_claims};
use super::{
    process_alive, read_exact_at, write_all_at, ByteClaim, CoordinatorState, FileLockCoordinator,
    HOLDER_SLOT_BASE, HOLDER_SLOT_COUNT, HOLDER_SLOT_SIZE, SLOT_METADATA_LOCK_BYTE, WAIT_SLOT_BASE,
    WAIT_SLOT_COUNT, WAIT_SLOT_SIZE,
};

impl FileLockCoordinator {
    /// No relation owner survives an epoch boundary. Clear relation metadata before generation numbers restart, preserving unrelated row waits and claims.
    pub(super) fn clear_relation_epoch_metadata(&self) -> Result<(), String> {
        loop {
            match self.apply_byte_mode(SLOT_METADATA_LOCK_BYTE, None, Some(true)) {
                Ok(()) => break,
                Err(error) if super::lock_would_block(&error) => {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                Err(error) => return Err(format!("admit relation metadata reset: {error}")),
            }
        }
        let result =
            (|| {
                for index in 0..HOLDER_SLOT_COUNT {
                    if self.read_holder_slot(index).is_some_and(|slot| {
                        super::super::relation_slot_of_claim(slot.offset).is_some()
                    }) {
                        write_all_at(
                            &self.file,
                            &[0; HOLDER_SLOT_SIZE as usize],
                            Self::holder_slot_offset(index),
                        )
                        .map_err(|error| format!("reset relation holder metadata: {error}"))?;
                    }
                }
                for index in 0..WAIT_SLOT_COUNT {
                    if self.read_slot(index).is_some_and(|slot| {
                        super::super::relation_slot_of_claim(slot.offset).is_some()
                    }) {
                        write_all_at(
                            &self.file,
                            &[0; WAIT_SLOT_SIZE as usize],
                            Self::slot_offset(index),
                        )
                        .map_err(|error| format!("reset relation wait metadata: {error}"))?;
                    }
                }
                Ok(())
            })();
        let unlocked = self
            .apply_byte_mode(SLOT_METADATA_LOCK_BYTE, Some(true), None)
            .map_err(|error| format!("release relation metadata reset: {error}"));
        result.and(unlocked)
    }
    fn holder_slot_offset(index: u64) -> u64 {
        HOLDER_SLOT_BASE + index * HOLDER_SLOT_SIZE
    }

    pub(super) fn read_holder_slot(&self, index: u64) -> Option<HolderSlot> {
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

    /// Publish the holder slots of every pending acquisition. The caller holds the slot metadata lock.
    fn publish_pending_holders(&self, state: &mut CoordinatorState) {
        state.pending_holders.clear();
        for (session, claim) in std::mem::take(&mut state.pending_order).into_values() {
            self.register_holder_slot(state, session, claim);
        }
    }

    #[cfg(test)]
    pub(super) fn publish_holders(&self) {
        let mut state = self.state.lock();
        self.acquire_slot_metadata_lock();
        self.publish_pending_holders(&mut state);
        let _ = self.apply_byte_mode(SLOT_METADATA_LOCK_BYTE, Some(true), None);
    }

    /// Write one acquisition's holder slot. The caller holds the slot metadata lock.
    pub(super) fn register_holder_slot(
        &self,
        state: &mut CoordinatorState,
        session: u64,
        claim: ByteClaim,
    ) {
        let pid = std::process::id();
        let mut reused = None;
        while let Some(index) = state.released_holder_slots.pop() {
            if !self.holder_slot_occupied(state, pid, index) {
                reused = Some(index);
                break;
            }
        }
        let preferred = state.next_holder_slot;
        let slot = reused.or_else(|| {
            (0..HOLDER_SLOT_COUNT).find_map(|probe| {
                let index = (preferred + probe) % HOLDER_SLOT_COUNT;
                (!self.holder_slot_occupied(state, pid, index)).then_some(index)
            })
        });
        if let Some(index) = slot {
            if reused.is_none() {
                state.next_holder_slot = (index + 1) % HOLDER_SLOT_COUNT;
            }
            self.write_holder_slot(
                index,
                Some(&HolderSlot {
                    pid,
                    session,
                    offset: claim.offset,
                    write: claim.write,
                    generation: Self::local_relation_generation(state, claim.offset),
                }),
            );
            state
                .holder_slots
                .entry((session, claim.offset, claim.write))
                .or_default()
                .push(index);
            state.occupied_holder_slots[index as usize] = true;
        }
    }

    fn holder_slot_occupied(&self, state: &CoordinatorState, pid: u32, index: u64) -> bool {
        state.occupied_holder_slots[index as usize]
            || self
                .read_holder_slot(index)
                .is_some_and(|existing| existing.pid != pid && process_alive(existing.pid))
    }

    /// Clear the holder slots of `claims` under one slot metadata lock. Each lock operation walks every record lock of the sidecar file, so a bulk release takes the metadata lock once rather than once per claim.
    pub(super) fn clear_holder_slots(
        &self,
        state: &mut CoordinatorState,
        session: u64,
        claims: &[ByteClaim],
    ) {
        let mut locked = false;
        for claim in claims {
            let key = (session, claim.offset, claim.write);
            // An unpublished acquisition has no slot to clear.
            if let Some(pending) = state.pending_holders.get_mut(&key) {
                let sequence = pending.pop();
                if pending.is_empty() {
                    state.pending_holders.remove(&key);
                }
                if let Some(sequence) = sequence {
                    state.pending_order.remove(&sequence);
                    continue;
                }
            }
            let Some(slots) = state.holder_slots.get_mut(&key) else {
                continue;
            };
            let Some(index) = slots.pop() else {
                continue;
            };
            if slots.is_empty() {
                state.holder_slots.remove(&key);
            }
            state.occupied_holder_slots[index as usize] = false;
            if !locked {
                self.acquire_slot_metadata_lock();
                locked = true;
            }
            self.write_holder_slot(index, None);
            // Allocation rechecks foreign process liveness before reusing this slot. The ordinary probe cursor keeps its position beyond live local holders.
            state.released_holder_slots.push(index);
        }
        if locked {
            let _ = self.apply_byte_mode(SLOT_METADATA_LOCK_BYTE, Some(true), None);
        }
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

    /// Advertise what one session of this process is currently waiting for so other processes can walk the wait-for graph. Each waiting session owns its own slot; slot exhaustion degrades detection, never coordination.
    pub(in crate::row_locks) fn register_wait(&self, session: u64, claim: ByteClaim) {
        let pid = std::process::id();
        let mut state = self.state.lock();
        let slot = WaitSlot {
            pid,
            session,
            offset: claim.offset,
            write: claim.write,
            generation: Self::local_relation_generation(&state, claim.offset),
            row: claim.row,
        };
        self.acquire_slot_metadata_lock();
        self.publish_pending_holders(&mut state);
        if let Some(index) = state.wait_slots.get(&session).copied() {
            self.write_slot(index, Some(&slot));
            let _ = self.apply_byte_mode(SLOT_METADATA_LOCK_BYTE, Some(true), None);
            return;
        }
        let preferred = (u64::from(pid).wrapping_mul(31).wrapping_add(session)) % WAIT_SLOT_COUNT;
        for probe in 0..WAIT_SLOT_COUNT {
            let index = (preferred + probe) % WAIT_SLOT_COUNT;
            let occupied = self.read_slot(index).is_some_and(|existing| {
                if existing.pid == pid {
                    // A slot of this process is live only while one of our sessions still owns it; stale slots from an earlier incarnation of this pid are reusable.
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

    /// Walk the cross-process wait-for graph from `wanted`, requested by local `session`. Foreign edges come from exact `(pid, session)` holder slots and the advertised wait of that same session. A byte held by this process is attributed to its local holder sessions: reaching the requesting session closes the cycle, an idle local holder ends that branch without a cycle, and a local holder that is itself waiting continues through `local_wait`, which reports the foreign byte a local session waits on, if any.
    pub(in crate::row_locks) fn wait_cycle_reaches_session(
        &self,
        session: u64,
        wanted: ByteClaim,
        local_wait: &dyn Fn(u64) -> Option<ByteClaim>,
    ) -> bool {
        let own_pid = std::process::id();
        let Some(generation) = self.current_relation_generation(wanted.offset) else {
            return false;
        };
        let mut pending = vec![(own_pid, session, wanted, generation)];
        let mut seen_sessions = std::collections::HashSet::new();
        while let Some((requester_pid, requester, current, generation)) = pending.pop() {
            // A foreign waiter can exit and its slot can be reused during traversal. Carry its observed generation through the edge instead of following the new identity at that byte.
            if self.current_relation_generation(current.offset) != Some(generation) {
                continue;
            }
            if !seen_sessions.insert((requester_pid, requester)) {
                continue;
            }
            for holder in self.local_holders_conflicting(current, generation) {
                if requester_pid == own_pid && holder == requester {
                    continue;
                }
                if holder == session {
                    return true;
                }
                if let Some(next) = local_wait(holder) {
                    if let Some(generation) = self.current_relation_generation(next.offset) {
                        pending.push((own_pid, holder, next, generation));
                    }
                }
            }
            for holder in self.holder_sessions(current, generation) {
                if holder.pid == own_pid
                    || (holder.pid == requester_pid && holder.session == requester)
                {
                    continue;
                }
                if let Some((wait, generation)) = self.wait_of(holder.pid, holder.session) {
                    pending.push((holder.pid, holder.session, wait, generation));
                }
            }
        }
        false
    }

    /// Local sessions holding a conflicting physical byte or relation mode.
    fn local_holders_conflicting(&self, claim: ByteClaim, generation: u64) -> Vec<u64> {
        let state = self.state.lock();
        if Self::local_relation_generation(&state, claim.offset) != generation {
            return Vec::new();
        }
        if row_claim_address(claim).is_some() {
            return state.rows.holders(claim);
        }
        let mut holders = Vec::new();
        for blocking in wait_blocking_claims(claim) {
            let Some(counts) = state.claims.get(&blocking.offset) else {
                continue;
            };
            if counts.exclusive > 0 || (blocking.write && counts.shared > 0) {
                if let Some(sessions) = state.holders.get(&blocking.offset) {
                    holders.extend(sessions.iter().copied());
                }
            }
        }
        holders.sort_unstable();
        holders.dedup();
        holders
    }

    fn holder_sessions(&self, claim: ByteClaim, generation: u64) -> Vec<HolderSlot> {
        if row_claim_address(claim).is_some() {
            return self.foreign_row_holders(claim);
        }
        let mut holders = Vec::new();
        let blocking = wait_blocking_claims(claim);
        for index in 0..HOLDER_SLOT_COUNT {
            if let Some(holder) = self.read_holder_slot(index) {
                if blocking
                    .clone()
                    .any(|claim| holder.offset == claim.offset && (holder.write || claim.write))
                    && generation == holder.generation
                    && process_alive(holder.pid)
                {
                    holders.push(holder);
                }
            }
        }
        holders
    }

    fn wait_of(&self, pid: u32, session: u64) -> Option<(ByteClaim, u64)> {
        for index in 0..WAIT_SLOT_COUNT {
            if let Some(slot) = self.read_slot(index) {
                if slot.pid == pid
                    && slot.session == session
                    && self.current_relation_generation(slot.offset) == Some(slot.generation)
                {
                    return Some((
                        ByteClaim {
                            offset: slot.offset,
                            write: slot.write,
                            row: slot.row,
                        },
                        slot.generation,
                    ));
                }
            }
        }
        None
    }
}

struct WaitSlot {
    row: Option<super::super::RowIdentity>,
    pid: u32,
    session: u64,
    offset: u64,
    write: bool,
    generation: u64,
}

#[cfg(test)]
mod tests;

pub(super) struct HolderSlot {
    pub(super) pid: u32,
    pub(super) session: u64,
    pub(super) offset: u64,
    pub(super) write: bool,
    pub(super) generation: u64,
}

impl HolderSlot {
    const MAGIC: u32 = 0x5551_484d;

    fn encode(&self) -> [u8; HOLDER_SLOT_SIZE as usize] {
        let mut bytes = [0_u8; HOLDER_SLOT_SIZE as usize];
        bytes[0..4].copy_from_slice(&Self::MAGIC.to_be_bytes());
        bytes[4..8].copy_from_slice(&self.pid.to_be_bytes());
        bytes[8..16].copy_from_slice(&self.offset.to_be_bytes());
        bytes[16..24]
            .copy_from_slice(&((self.generation << 1) | u64::from(self.write)).to_be_bytes());
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
            write: bytes[23] & 1 != 0,
            generation: u64::from_be_bytes(bytes[16..24].try_into().ok()?) >> 1,
        })
    }
}

impl WaitSlot {
    const MAGIC: u32 = 0x5551_4c4d;

    fn encode(&self) -> [u8; WAIT_SLOT_SIZE as usize] {
        let mut bytes = [0_u8; WAIT_SLOT_SIZE as usize];
        bytes[0..4].copy_from_slice(&Self::MAGIC.to_be_bytes());
        bytes[4..8].copy_from_slice(&self.pid.to_be_bytes());
        bytes[8..16].copy_from_slice(&self.offset.to_be_bytes());
        bytes[16..24]
            .copy_from_slice(&((self.generation << 1) | u64::from(self.write)).to_be_bytes());
        bytes[24..32].copy_from_slice(&self.session.to_be_bytes());
        if let Some(row) = self.row {
            bytes[32..80].copy_from_slice(&row.encode());
        }
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
        let row = if bytes[8] & 0x80 != 0 {
            Some(super::super::RowIdentity::decode(&bytes[32..80])?)
        } else {
            None
        };
        Some(Self {
            row,
            pid,
            session: u64::from_be_bytes(bytes[24..32].try_into().ok()?),
            offset: u64::from_be_bytes(bytes[8..16].try_into().ok()?),
            write: bytes[23] & 1 != 0,
            generation: u64::from_be_bytes(bytes[16..24].try_into().ok()?) >> 1,
        })
    }
}
