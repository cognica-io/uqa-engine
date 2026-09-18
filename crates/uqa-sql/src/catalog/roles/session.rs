//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Session authorization identities and independently restorable role settings.

use super::{identity::RoleBinding, RoleDefinition};
use std::sync::Arc;

#[derive(Clone)]
pub struct SessionAuthorization {
    authenticated: Arc<RoleBinding>,
    session: Arc<RoleBinding>,
    selected: Option<Arc<RoleBinding>>,
    effective: Arc<RoleBinding>,
}

impl Default for SessionAuthorization {
    fn default() -> Self {
        Self::new(
            RoleBinding::from_definition(&RoleDefinition::bootstrap())
                .expect("bootstrap role identity"),
        )
    }
}

impl SessionAuthorization {
    pub fn new(authenticated: RoleBinding) -> Self {
        let authenticated = Arc::new(authenticated);
        Self {
            session: authenticated.clone(),
            effective: authenticated.clone(),
            authenticated,
            selected: None,
        }
    }

    pub fn authenticated(&self) -> &Arc<RoleBinding> {
        &self.authenticated
    }

    pub fn session(&self) -> &Arc<RoleBinding> {
        &self.session
    }

    pub fn current(&self) -> &Arc<RoleBinding> {
        &self.effective
    }

    pub fn selected(&self) -> Option<&Arc<RoleBinding>> {
        self.selected.as_ref()
    }

    pub fn set_role(&mut self, selected: Option<Arc<RoleBinding>>) {
        self.effective = selected.as_ref().unwrap_or(&self.session).clone();
        self.selected = selected;
    }

    /// Session and role assignment commute during transaction-local restoration.
    pub fn restore_session(&mut self, session: Arc<RoleBinding>) {
        if self.selected.is_none() {
            self.effective = session.clone();
        }
        self.session = session;
    }

    pub fn set_session(&mut self, session: Arc<RoleBinding>) {
        self.restore_session(session);
        self.set_role(None);
    }

    pub fn set_effective(&mut self, role: Arc<RoleBinding>) {
        self.effective = role;
    }

    pub fn discard(&mut self) {
        self.set_session(self.authenticated.clone());
    }

    pub fn show_role(&self) -> &str {
        self.selected
            .as_ref()
            .map_or("none", |role| role.name.as_str())
    }
}

#[cfg(test)]
mod tests;
