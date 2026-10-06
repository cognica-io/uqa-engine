//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Authorize immutable routine identities against one retained ACL and role snapshot.

use crate::catalog::{
    context::CatalogContext,
    projection::{builtin_routine_identities, catalog_routine_type_oid, BuiltinRoutineIdentity},
    CatalogReadView,
};
use uqa_sql::{
    ast::FunctionBinding,
    catalog::{
        roles::{identity::RoleSubject, role_inherits, RoleReference},
        security::{builtin_routines::BuiltinRoutineExecution, object_acl},
    },
    SQLError,
};

#[derive(Clone)]
pub struct BuiltinRoutinePermissions {
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
        Self {
            catalog,
            current_role,
        }
    }

    #[must_use]
    pub fn capture(context: &CatalogContext<'_>) -> Self {
        Self::new(context.catalog_read_view(), context.current_role())
    }

    #[must_use]
    pub fn unrestricted(&self) -> bool {
        let definitions = &self.catalog.snapshot().definitions;
        definitions.builtin_routine_security.is_empty()
            || self
                .current_role
                .role_definition(&definitions.roles)
                .is_some_and(|role| {
                    role.attributes
                        .contains(&uqa_sql::ast::RoleAttribute::Superuser)
                })
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
        let definitions = &self.catalog.snapshot().definitions;
        let acl = definitions
            .builtin_routine_security
            .get(&routine.oid)
            .map(|entry| entry.execute_acl.as_slice());
        let allowed = object_acl::privilege_allowed(
            &uqa_core::catalog_role::RoleIdentity::BOOTSTRAP,
            acl,
            false,
            self.unrestricted(),
            |role| {
                role_inherits(
                    &definitions.roles,
                    &definitions.role_memberships,
                    &self.current_role,
                    role,
                )
            },
        );
        if allowed {
            return Ok(());
        }
        let kind = if routine.kind == 'a' {
            "aggregate"
        } else {
            "function"
        };
        Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: format!("permission denied for {kind} {}", routine.name),
        })
    }

    /// Match the declaration carried by the selected binding, not argument values or the caller's current search path. User identities and compiler syntax dispatch never borrow the ACL of a same-named catalog function.
    fn selected(&self, binding: &FunctionBinding) -> Option<BuiltinRoutineIdentity> {
        if !binding.builtin || binding.object_id.is_some() || binding.resolution_error.is_some() {
            return None;
        }
        let (schema, name) = uqa_core::RelationIdentity::parse_reference(&binding.name).ok()?;
        if schema
            .as_deref()
            .is_some_and(|schema| schema != "pg_catalog")
        {
            return None;
        }
        builtin_routine_identities().find(|routine| {
            routine.name == name
                && routine.argument_types.len() == binding.argument_types.len()
                && routine
                    .argument_types
                    .iter()
                    .zip(&binding.argument_types)
                    .all(|(oid, name)| *oid == catalog_routine_type_oid(&self.catalog, name))
        })
    }
}

impl BuiltinRoutineExecution for BuiltinRoutinePermissions {
    fn require_execute(&self, binding: &FunctionBinding) -> Result<(), SQLError> {
        if self.unrestricted() {
            return Ok(());
        }
        if let Some(routine) = self.selected(binding) {
            self.require_selected(routine)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
