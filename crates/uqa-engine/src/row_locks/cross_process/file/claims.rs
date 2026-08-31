//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Atomic cross-process byte claims and release accounting.

use super::{lock_would_block, ByteClaim, CoordinatorState, FileLockCoordinator};

impl FileLockCoordinator {
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
            // Downgrading or unlocking a held range cannot block; an I/O-level failure here would leave a stricter record lock in place, which is conservative rather than unsound.
            let _ = self.apply_byte_mode(claim.offset, before, after);
        }
    }

    /// Try to add every claim without blocking. Either all claims are applied, or none are and the contended claim is reported.
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
}
