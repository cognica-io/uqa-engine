//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Atomic admission for the eight `PostgreSQL` relation-lock conflict sets.

use super::super::{
    relation_byte_claims, relation_mode_claim, RelationClaimWait, RelationLockMode,
};
use super::{lock_would_block, CoordinatorState, FileLockCoordinator};

const RELATION_ADMISSION_BYTE: u64 = 14;

struct Admission<'a> {
    coordinator: &'a FileLockCoordinator,
    active: bool,
}

impl Admission<'_> {
    fn release(&mut self) -> Result<(), String> {
        self.coordinator
            .apply_byte_mode(RELATION_ADMISSION_BYTE, Some(true), None)
            .map_err(|error| format!("release relation-lock admission: {error}"))?;
        self.active = false;
        Ok(())
    }
}

impl Drop for Admission<'_> {
    fn drop(&mut self) {
        if self.active {
            let _ = self.release();
        }
    }
}

impl FileLockCoordinator {
    pub(in crate::row_locks) fn try_relation_claim(
        &self,
        session: u64,
        relation: &[u8],
        mode: RelationLockMode,
    ) -> Result<Result<(), RelationClaimWait>, String> {
        let mut state = self.state.lock();
        if let Err(error) = self.apply_byte_mode(RELATION_ADMISSION_BYTE, None, Some(true)) {
            return if lock_would_block(&error) {
                Ok(Err(RelationClaimWait::AdmissionBusy))
            } else {
                Err(format!("acquire relation-lock admission: {error}"))
            };
        }
        let mut admission = Admission {
            coordinator: self,
            active: true,
        };
        let result = self.try_admitted_relation(&mut state, session, relation, mode);
        if let Err(error) = admission.release() {
            if matches!(result, Ok(Ok(()))) {
                for claim in relation_byte_claims(relation, mode).into_iter().rev() {
                    self.release_one(&mut state, session, claim);
                }
            }
            return Err(error);
        }
        result
    }

    fn try_admitted_relation(
        &self,
        state: &mut CoordinatorState,
        session: u64,
        relation: &[u8],
        mode: RelationLockMode,
    ) -> Result<Result<(), RelationClaimWait>, String> {
        for held in RelationLockMode::ALL {
            if !mode.conflicts_with(held) {
                continue;
            }
            let claim = relation_mode_claim(relation, held, true);
            if state
                .holders
                .get(&claim.offset)
                .is_some_and(|holders| holders.iter().any(|holder| *holder != session))
            {
                return Ok(Err(RelationClaimWait::Conflict(claim)));
            }
            // POSIX locks belong to the process. An exclusive probe must preserve this session's own shared holder while still detecting foreign holders of the same byte.
            let previous = state
                .claims
                .get(&claim.offset)
                .and_then(super::ByteClaimCounts::mode);
            if let Err(error) = self.apply_byte_mode(claim.offset, previous, Some(true)) {
                return if lock_would_block(&error) {
                    Ok(Err(RelationClaimWait::Conflict(claim)))
                } else {
                    Err(format!("probe relation-lock holders: {error}"))
                };
            }
            self.apply_byte_mode(claim.offset, Some(true), previous)
                .map_err(|error| format!("restore relation-lock holder after probe: {error}"))?;
        }
        self.try_claim_in(state, session, &relation_byte_claims(relation, mode))
            .map(|claim| claim.map_err(RelationClaimWait::Conflict))
    }
}

#[cfg(test)]
mod tests;
