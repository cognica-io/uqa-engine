//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Planning authority owns its wrapper and observes retained ACL inputs through admitted lookup work.

use super::{BuiltinRoutineExecution, BuiltinRoutineIdentity, BuiltinRoutinePermissions};
use crate::catalog::{context::CatalogContext, projection::builtin_routine_identities};
use std::sync::Arc;
use uqa_core::{
    memory::{Produced, ProductionControl},
    RelationIdentity, RelationName,
};
use uqa_sql::{
    ast::FunctionBinding,
    catalog::{roles::memberships::role_inherits_with_control, security::object_acl},
    SQLError,
};

#[derive(Debug)]
struct ControlledPermissions(Produced<BuiltinRoutinePermissions>);

impl BuiltinRoutinePermissions {
    pub fn shared_with_control(
        context: &CatalogContext<'_>,
        control: &ProductionControl<'_>,
    ) -> Result<Arc<dyn BuiltinRoutineExecution>, SQLError> {
        let memory = control.reserve(size_of::<ControlledPermissions>())?;
        let permissions = control.finish(Self::capture(context), memory)?;
        Ok(Arc::new(ControlledPermissions(permissions)))
    }

    pub(super) fn require_selected_with_control(
        &self,
        routine: BuiltinRoutineIdentity,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        let Some(authority) = &self.restricted else {
            return Ok(());
        };
        let definitions = &authority.catalog.snapshot().definitions;
        let acl = definitions
            .builtin_routine_security
            .get(&routine.oid)
            .map(|entry| entry.execute_acl.as_slice());
        let allowed = object_acl::try_privilege_allowed(
            &uqa_core::catalog_role::RoleIdentity::BOOTSTRAP,
            acl,
            false,
            false,
            |role| {
                role_inherits_with_control(
                    &definitions.roles,
                    &definitions.role_memberships,
                    &authority.current_role,
                    role,
                    control,
                )
            },
        )?;
        if allowed {
            return Ok(());
        }
        let kind = if routine.kind == 'a' {
            "aggregate"
        } else {
            "function"
        };
        let (message, _memory) = control
            .format(format_args!(
                "permission denied for {kind} {}",
                routine.name,
            ))?
            .into_parts();
        Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message,
        })
    }

    fn selected_with_control(
        &self,
        binding: &FunctionBinding,
        control: &ProductionControl<'_>,
    ) -> Result<Option<BuiltinRoutineIdentity>, SQLError> {
        control.check()?;
        let Some(authority) = &self.restricted else {
            return Ok(None);
        };
        if !binding.builtin || binding.object_id.is_some() || binding.resolution_error.is_some() {
            return Ok(None);
        }
        let Some(reference) = RelationIdentity::borrowed_reference(&binding.name) else {
            return Ok(None);
        };
        if reference
            .schema
            .is_some_and(|schema| schema != RelationName::decoded("pg_catalog"))
        {
            return Ok(None);
        }
        for routine in builtin_routine_identities() {
            control.check()?;
            if reference.name != RelationName::decoded(routine.name)
                || routine.argument_types.len() != binding.argument_types.len()
            {
                continue;
            }
            let mut matches = true;
            for (expected, name) in routine.argument_types.iter().zip(&binding.argument_types) {
                if *expected
                    != crate::catalog::projection::catalog_routine_type_oid_with_control(
                        &authority.catalog,
                        name,
                        control,
                    )?
                {
                    matches = false;
                    break;
                }
            }
            if matches {
                return Ok(Some(routine));
            }
        }
        Ok(None)
    }

    pub(super) fn require_with_control(
        &self,
        binding: &FunctionBinding,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        control.check()?;
        if let Some(routine) = self.selected_with_control(binding, control)? {
            self.require_selected_with_control(routine, control)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;

impl BuiltinRoutineExecution for ControlledPermissions {
    fn require_execute(&self, binding: &FunctionBinding) -> Result<(), SQLError> {
        self.0.require_execute(binding)
    }

    fn require_execute_with_control(
        &self,
        binding: &FunctionBinding,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        self.0.require_with_control(binding, control)
    }
}
