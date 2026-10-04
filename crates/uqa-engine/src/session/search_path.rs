//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The schemas a session's search path names.

use parking_lot::RwLockReadGuard;
use std::ops::Deref;

use crate::SessionStateSnapshot;

/// The search path of a session that has not set one.
pub(crate) fn default_search_path() -> Vec<String> {
    vec!["$user".into(), "public".into()]
}

/// The schemas the session's search path names, with `$user` standing for the current role's name (`recomputeNamespacePath`). Schemas that do not exist or that the role may not use are skipped where names are looked up.
pub(crate) fn effective_search_path(state: &SessionStateSnapshot) -> Vec<String> {
    state
        .search_path
        .iter()
        .map(|schema| {
            if schema == "$user" {
                state.authorization.current().name.clone()
            } else {
                schema.clone()
            }
        })
        .collect()
}

/// The session's effective search path, read while the session's values stay locked for reading, so that names a statement resolves against it see one search path and role.
pub(crate) struct LockedSearchPath<'a> {
    _state: RwLockReadGuard<'a, SessionStateSnapshot>,
    path: Vec<String>,
}

impl<'a> LockedSearchPath<'a> {
    pub(crate) fn new(state: RwLockReadGuard<'a, SessionStateSnapshot>) -> Self {
        let path = effective_search_path(&state);
        Self {
            _state: state,
            path,
        }
    }
}

impl Deref for LockedSearchPath<'_> {
    type Target = Vec<String>;

    fn deref(&self) -> &Self::Target {
        &self.path
    }
}
