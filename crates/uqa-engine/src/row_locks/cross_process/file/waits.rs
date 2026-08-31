//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Holder and waiter slots plus cross-process wait-graph traversal.

use super::{
    process_alive, read_exact_at, write_all_at, ByteClaim, CoordinatorState, FileLockCoordinator,
    HOLDER_SLOT_BASE, HOLDER_SLOT_COUNT, HOLDER_SLOT_SIZE, SLOT_METADATA_LOCK_BYTE, WAIT_SLOT_BASE,
    WAIT_SLOT_COUNT, WAIT_SLOT_SIZE,
};

impl FileLockCoordinator {
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

    pub(super) fn register_holder_slot(
        &self,
        state: &mut CoordinatorState,
        session: u64,
        claim: ByteClaim,
    ) {
        self.acquire_slot_metadata_lock();
        let pid = std::process::id();
        let preferred = state.next_holder_slot;
        let slot = (0..HOLDER_SLOT_COUNT).find_map(|probe| {
            let index = (preferred + probe) % HOLDER_SLOT_COUNT;
            let occupied = state.occupied_holder_slots[index as usize]
                || self
                    .read_holder_slot(index)
                    .is_some_and(|existing| existing.pid != pid && process_alive(existing.pid));
            (!occupied).then_some(index)
        });
        if let Some(index) = slot {
            state.next_holder_slot = (index + 1) % HOLDER_SLOT_COUNT;
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
            state.occupied_holder_slots[index as usize] = true;
        }
        let _ = self.apply_byte_mode(SLOT_METADATA_LOCK_BYTE, Some(true), None);
    }

    pub(super) fn clear_holder_slot(
        &self,
        state: &mut CoordinatorState,
        session: u64,
        claim: ByteClaim,
    ) {
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
        state.occupied_holder_slots[index as usize] = false;
        self.acquire_slot_metadata_lock();
        self.write_holder_slot(index, None);
        let _ = self.apply_byte_mode(SLOT_METADATA_LOCK_BYTE, Some(true), None);
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

    /// Local sessions whose claims of `claim.offset` conflict with the requested claim.
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
