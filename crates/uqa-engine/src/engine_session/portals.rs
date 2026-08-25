//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Session portal lifecycle shared by PL/pgSQL routine activations.

use crate::{Engine, SQLError, SQLResult, SessionPortalState, Value};

impl Engine {
    pub(crate) fn allocate_session_portal_name(&self) -> String {
        let mut next = self.session.next_portal_id.lock();
        let name = format!("<unnamed portal {}>", *next);
        *next += 1;
        name
    }

    pub(crate) fn open_session_portal(
        &self,
        name: String,
        result: SQLResult,
    ) -> Result<(), SQLError> {
        let mut portals = self.session.portals.lock();
        if portals.contains_key(&name) {
            return Err(cursor_error(&name, "already in use", "42P03"));
        }
        portals.insert(
            name,
            SessionPortalState {
                result,
                position: 0,
            },
        );
        Ok(())
    }

    pub(crate) fn ensure_session_portal_available(&self, name: &str) -> Result<(), SQLError> {
        if self.session.portals.lock().contains_key(name) {
            return Err(cursor_error(name, "already in use", "42P03"));
        }
        Ok(())
    }

    pub(crate) fn fetch_session_portal_next(
        &self,
        name: &str,
    ) -> Result<(Vec<String>, Option<Vec<Value>>), SQLError> {
        let mut portals = self.session.portals.lock();
        let state = portals
            .get_mut(name)
            .ok_or_else(|| cursor_error(name, "does not exist", "34000"))?;
        let values = (state.position < state.result.rows.len()).then(|| {
            (0..state.result.columns.len())
                .map(|column| {
                    state
                        .result
                        .value_at(state.position, column)
                        .cloned()
                        .unwrap_or(Value::Null)
                })
                .collect()
        });
        if values.is_some() {
            state.position += 1;
        }
        Ok((state.result.columns.clone(), values))
    }

    pub(crate) fn close_session_portal(&self, name: &str) -> Result<(), SQLError> {
        if self.session.portals.lock().remove(name).is_none() {
            return Err(cursor_error(name, "does not exist", "34000"));
        }
        Ok(())
    }
}

fn cursor_error(name: &str, message: &str, sqlstate: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: sqlstate.into(),
        message: format!("cursor \"{name}\" {message}"),
    }
}
