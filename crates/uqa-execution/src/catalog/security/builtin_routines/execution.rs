//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Authorize immutable routine identities against one retained ACL and role snapshot.

use crate::catalog::{
    context::CatalogContext,
    projection::{builtin_routine_identities, BuiltinRoutineIdentity},
    CatalogReadView,
};
use std::collections::BTreeMap;
use uqa_sql::{
    ast::FunctionBinding,
    catalog::{
        roles::{identity::RoleSubject, RoleDefinition, RoleReference},
        security::builtin_routines::{BuiltinRoutineExecution, BuiltinRoutineSecurities},
    },
    SQLError,
};

mod production;

#[derive(Clone)]
pub struct BuiltinRoutinePermissions {
    restricted: Option<RestrictedAuthority>,
}

#[derive(Clone)]
struct RestrictedAuthority {
    catalog: CatalogReadView,
    current_role: RoleReference,
}

impl std::fmt::Debug for BuiltinRoutinePermissions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BuiltinRoutinePermissions")
            .finish_non_exhaustive()
    }
}

impl BuiltinRoutinePermissions {
    #[must_use]
    pub fn new(catalog: CatalogReadView, current_role: RoleReference) -> Self {
        let definitions = &catalog.snapshot().definitions;
        if let Some(authority) = Self::try_unrestricted(
            &definitions.builtin_routine_security,
            &definitions.roles,
            &current_role,
        ) {
            return authority;
        }
        Self {
            restricted: Some(RestrictedAuthority {
                catalog,
                current_role,
            }),
        }
    }

    #[must_use]
    pub fn capture(context: &CatalogContext<'_>) -> Self {
        context
            .catalog
            .builtin_routine_permissions(context.current_role())
    }

    #[must_use]
    pub fn unrestricted(&self) -> bool {
        self.restricted.is_none()
    }

    /// The unrestricted decision uses only immutable ACL overrides and role attributes; table and type definitions cannot change it.
    #[must_use]
    pub fn try_unrestricted(
        security: &BuiltinRoutineSecurities,
        roles: &BTreeMap<String, RoleDefinition>,
        current_role: &RoleReference,
    ) -> Option<Self> {
        (security.is_empty()
            || current_role.role_definition(roles).is_some_and(|role| {
                role.attributes
                    .contains(&uqa_sql::ast::RoleAttribute::Superuser)
            }))
        .then_some(Self { restricted: None })
    }

    pub fn require_analyzed_set_call(
        &self,
        name: &str,
        arguments: &[Option<uqa_sql::ColumnType>],
        window: bool,
    ) -> Result<(), SQLError> {
        if self.unrestricted() {
            return Ok(());
        }
        if let Some(routine) =
            uqa_sql::catalog::security::builtin_routines::selection::select_set_call(
                name,
                arguments,
                window,
                builtin_routine_identities(),
            )?
        {
            self.require_selected(routine)?;
        }
        Ok(())
    }

    fn require_selected(&self, routine: BuiltinRoutineIdentity) -> Result<(), SQLError> {
        self.require_selected_with_control(
            routine,
            &uqa_core::memory::ProductionControl::uncontrolled(),
        )
    }
}

impl BuiltinRoutineExecution for BuiltinRoutinePermissions {
    fn require_execute(&self, binding: &FunctionBinding) -> Result<(), SQLError> {
        self.require_with_control(
            binding,
            &uqa_core::memory::ProductionControl::uncontrolled(),
        )
    }

    fn require_execute_with_control(
        &self,
        binding: &FunctionBinding,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        self.require_with_control(binding, control)
    }
}

#[cfg(test)]
mod tests;
