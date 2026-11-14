//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Session namespace selection retains one state generation before any owned names are produced.

use super::SessionContext;
use std::sync::Arc;
use uqa_core::memory::{Produced, ProductionControl};
use uqa_sql::{
    catalog::{
        resolution::{RelationLookupMode, RelationNameResolution},
        roles::{identity::RoleBinding, RoleReference},
    },
    SQLError,
};

pub(crate) struct SessionNamespace {
    pub(crate) search_path: Arc<Vec<String>>,
    pub(crate) role: Arc<RoleBinding>,
    pub(crate) session_id: u64,
    pub(crate) temporary_namespace_allocated: bool,
}

impl SessionContext {
    pub(crate) fn namespace_snapshot(&self, session_id: u64) -> SessionNamespace {
        let state = self.state.read();
        SessionNamespace {
            search_path: state.search_path.clone(),
            role: state.authorization.current().clone(),
            session_id,
            temporary_namespace_allocated: state.temporary_namespace.is_some(),
        }
    }
}

impl SessionNamespace {
    pub(crate) fn expanded_path(&self) -> impl Iterator<Item = &str> {
        self.search_path.iter().map(|name| {
            if name == "$user" {
                self.role.name.as_str()
            } else {
                name.as_str()
            }
        })
    }

    pub(crate) fn capture_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<RelationNameResolution>, SQLError> {
        RelationNameResolution::from_inputs_with_control(
            self.expanded_path(),
            control.format(format_args!("pg_temp_{}", self.session_id))?,
            self.temporary_namespace_allocated,
            &RoleReference::Bound(self.role.clone()),
            RelationLookupMode::Dynamic,
            control,
        )
    }
}

#[cfg(test)]
mod tests;
