//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Release of session, savepoint-marked, and provisional lock ownership.

use super::{remove_inactive_versions, RowLockAcquisition, RowLockManager};

impl RowLockManager {
    pub(crate) fn release_mark_above(&self, session_id: u64, mark: u32) {
        let mut released_rows = Vec::new();
        let mut released_relations = Vec::new();
        let mut state = self.state.lock();
        state.rows.retain(|key, grants| {
            grants.retain_mut(|grant| {
                if grant.session_id == session_id {
                    grant.acquisitions.retain(|acquisition| {
                        let keep = acquisition.mark <= mark;
                        if !keep {
                            released_rows.push((*key, acquisition.strength));
                        }
                        keep
                    });
                }
                !grant.acquisitions.is_empty()
            });
            !grants.is_empty()
        });
        state.relations.retain(|table, grants| {
            grants.retain_mut(|grant| {
                if grant.session_id == session_id {
                    grant.acquisitions.retain(|acquisition| {
                        let keep = acquisition.mark <= mark;
                        if !keep {
                            released_relations.push((*table, acquisition.mode));
                        }
                        keep
                    });
                }
                !grant.acquisitions.is_empty()
            });
            !grants.is_empty()
        });
        state.waiting.remove(&session_id);
        state.waiting_relations.remove(&session_id);
        state.advertised_waits.remove(&session_id);
        remove_inactive_versions(&mut state);
        drop(state);
        for (key, strength) in released_rows {
            self.release_row_claims(session_id, key, strength);
        }
        for (table, mode) in released_relations {
            self.release_relation_claims(session_id, table, mode);
        }
        self.wake.notify_all();
    }

    pub(crate) fn release_session(&self, session_id: u64) {
        let mut released_rows = Vec::new();
        let mut released_relations = Vec::new();
        let mut state = self.state.lock();
        state.rows.retain(|key, grants| {
            grants.retain(|grant| {
                if grant.session_id == session_id {
                    for acquisition in &grant.acquisitions {
                        released_rows.push((*key, acquisition.strength));
                    }
                    return false;
                }
                true
            });
            !grants.is_empty()
        });
        state.relations.retain(|table, grants| {
            grants.retain(|grant| {
                if grant.session_id == session_id {
                    for acquisition in &grant.acquisitions {
                        released_relations.push((*table, acquisition.mode));
                    }
                    return false;
                }
                true
            });
            !grants.is_empty()
        });
        state.waiting.remove(&session_id);
        state.waiting_relations.remove(&session_id);
        state.advertised_waits.remove(&session_id);
        remove_inactive_versions(&mut state);
        drop(state);
        for (key, strength) in released_rows {
            self.release_row_claims(session_id, key, strength);
        }
        for (table, mode) in released_relations {
            self.release_relation_claims(session_id, table, mode);
        }
        self.wake.notify_all();
    }

    pub(crate) fn rollback_acquisition(&self, acquisition: RowLockAcquisition) {
        let mut released = None;
        let mut state = self.state.lock();
        let Some(grants) = state.rows.get_mut(&acquisition.key) else {
            return;
        };
        grants.retain_mut(|grant| {
            if grant.session_id == acquisition.session_id {
                grant.acquisitions.retain(|marked| {
                    let keep = marked.acquisition_id != acquisition.acquisition_id;
                    if !keep {
                        released = Some(marked.strength);
                    }
                    keep
                });
            }
            !grant.acquisitions.is_empty()
        });
        if grants.is_empty() {
            state.rows.remove(&acquisition.key);
        }
        remove_inactive_versions(&mut state);
        drop(state);
        if let Some(strength) = released {
            self.release_row_claims(acquisition.session_id, acquisition.key, strength);
        }
        self.wake.notify_all();
    }
}
