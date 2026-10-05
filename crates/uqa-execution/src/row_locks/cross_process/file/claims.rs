//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Atomic cross-process byte claims and release accounting.

use super::super::row_claim_address;
use super::{lock_would_block, ByteClaim, CoordinatorState, FileLockCoordinator};

impl FileLockCoordinator {
    pub(super) fn release_one(&self, state: &mut CoordinatorState, session: u64, claim: ByteClaim) {
        self.clear_holder_slots(state, session, std::slice::from_ref(&claim));
        self.unlock_claim(state, session, claim);
    }

    /// Drop one claim's holder attribution and count, unlocking its byte when its mode changes. Its holder slot must already be clear.
    fn unlock_claim(&self, state: &mut CoordinatorState, session: u64, claim: ByteClaim) {
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
            // A failed unlock can leave a stricter native claim. Keep that relation identity pinned until the descriptor closes, so the slot cannot be reused for an unrelated relation.
            if self.apply_byte_mode(claim.offset, before, after).is_err() {
                Self::poison_relation_slot(state, claim.offset);
            }
        }
    }

    /// Try to add every claim without blocking. Either all claims are applied, or none are and the contended claim is reported.
    pub(in crate::row_locks) fn try_claim(
        &self,
        session: u64,
        claims: &[ByteClaim],
    ) -> Result<Result<(), ByteClaim>, String> {
        let mut state = self.state.lock();
        self.try_claim_in(&mut state, session, claims)
    }

    pub(super) fn try_claim_in(
        &self,
        state: &mut CoordinatorState,
        session: u64,
        claims: &[ByteClaim],
    ) -> Result<Result<(), ByteClaim>, String> {
        // One acquisition claims either rows or record-lock bytes, never both.
        if claims
            .first()
            .is_some_and(|claim| row_claim_address(*claim).is_some())
        {
            return self.try_claim_rows(state, session, claims);
        }
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
                        self.release_one(state, session, *undo);
                    }
                    if lock_would_block(&error) {
                        return Ok(Err(*claim));
                    }
                    return Err(format!("cross-process lock claim failed: {error}"));
                }
            }
            state.holders.entry(claim.offset).or_default().push(session);
            let sequence = state.next_pending;
            state.next_pending += 1;
            state.pending_order.insert(sequence, (session, *claim));
            state
                .pending_holders
                .entry((session, claim.offset, claim.write))
                .or_default()
                .push(sequence);
            applied.push(*claim);
        }
        Ok(Ok(()))
    }

    /// Release claims that were successfully applied earlier by `session`. Every holder slot is cleared before any byte is unlocked, so no waiter can acquire a byte while a slot still attributes it to `session`. The kernel keeps each process's record locks sorted by offset, so unlocking in ascending order finds each lock at the front of this process's locks. Row claims, whose addresses follow every record-lock offset, leave the claim table under one hold of its lock byte.
    pub(in crate::row_locks) fn release(&self, session: u64, claims: &[ByteClaim]) {
        let mut ordered = claims.to_vec();
        ordered.sort_unstable_by_key(|claim| (claim.offset, claim.write));
        let (records, rows) =
            ordered.split_at(ordered.partition_point(|claim| row_claim_address(*claim).is_none()));
        let mut state = self.state.lock();
        self.clear_holder_slots(&mut state, session, records);
        for claim in records {
            self.unlock_claim(&mut state, session, *claim);
        }
        self.release_rows(&mut state, session, rows);
    }
}
