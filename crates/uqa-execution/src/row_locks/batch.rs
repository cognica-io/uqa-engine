//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded acquisition of already evaluated reservations without intervening observations.

use super::{
    remove_inactive_versions, rollback_grant, row_byte_claims, try_grant, GrantAttempt,
    LockAcquire, LockRequest, RowLockManager, SQLError,
};

impl RowLockManager {
    /// Acquire pre-evaluated reservations in their original order. A group with no conflicting holder publishes all its grants under one claim-table lock. A conflict leaves that attempt unchanged and uses ordinary single-request waits, preserving prefix retention, wait diagnostics and deadlock detection. Callers must not put observable work between requests and must refresh their data view unconditionally after the batch; preliminary conflicts need not cause a later individual wait.
    pub fn acquire_batch(
        &self,
        requests: &[LockRequest<'_>],
    ) -> Result<Vec<LockAcquire>, SQLError> {
        let mut results = Vec::with_capacity(requests.len());
        for chunk in requests.chunks(64) {
            if let Some(granted) = self.try_grant_batch(chunk)? {
                results.extend(granted);
            } else {
                for request in chunk {
                    results.push(self.acquire(request)?);
                }
            }
        }
        Ok(results)
    }

    fn try_grant_batch(
        &self,
        requests: &[LockRequest<'_>],
    ) -> Result<Option<Vec<LockAcquire>>, SQLError> {
        let session = requests[0].session_id;
        if requests.len() == 1 || requests.iter().any(|request| request.session_id != session) {
            return Ok(None);
        }
        let coordinator = self.coordinator()?;
        let relations = requests
            .iter()
            .map(|request| self.relation_bytes(request.key.table))
            .collect::<Vec<_>>();
        let mut identities = Vec::with_capacity(requests.len());
        for (request, relation) in requests.iter().zip(&relations) {
            request.cancel.check()?;
            identities.push(
                coordinator
                    .map(|coordinator| {
                        coordinator.pin_row(relation, request.key.doc_id, request.cancel)
                    })
                    .transpose()?,
            );
        }
        let mut state = self.state.lock();
        let mut acquisitions = Vec::with_capacity(requests.len());
        let mut claims = Vec::with_capacity(requests.len() * 2);
        let attempt = (|| {
            for (request, identity) in requests.iter().zip(&identities) {
                request.cancel.check()?;
                match try_grant(
                    &mut state,
                    session,
                    request.key,
                    request.strength,
                    request.mark,
                    &self.next_acquisition,
                ) {
                    GrantAttempt::Conflict => return Ok(false),
                    GrantAttempt::Granted(acquisition) => {
                        if acquisition.is_some() {
                            if let Some(identity) = identity {
                                claims
                                    .extend(row_byte_claims(identity.identity(), request.strength));
                            }
                        }
                        acquisitions.push(acquisition);
                    }
                }
            }
            if let Some(coordinator) = coordinator.filter(|_| !claims.is_empty()) {
                return coordinator
                    .try_claim(session, &claims)
                    .map(|result| result.is_ok())
                    .map_err(SQLError::Internal);
            }
            Ok(true)
        })();
        if !matches!(attempt, Ok(true)) {
            for acquisition in acquisitions.into_iter().rev().flatten() {
                rollback_grant(&mut state, acquisition);
            }
            return attempt.map(|_| None);
        }
        for (identity, acquisition) in identities.iter_mut().zip(&acquisitions) {
            if acquisition.is_some() {
                if let Some(identity) = identity {
                    identity.retain();
                }
            }
        }
        state.waiting.remove(&session);
        remove_inactive_versions(&mut state);
        Ok(Some(
            acquisitions
                .into_iter()
                .map(|acquisition| LockAcquire::Granted {
                    waited: false,
                    foreign_waited: false,
                    acquisition,
                })
                .collect(),
        ))
    }
}

#[cfg(test)]
mod tests;
