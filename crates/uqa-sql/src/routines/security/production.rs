//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine EXECUTE checks admit their name and propagate membership production failures.

use super::{
    bound_routine_owner, object_acl, routine_kind, CreateFunction, RoutineExecutionAuthority,
    SQLError,
};
use uqa_core::memory::ProductionControl;

#[cfg(test)]
mod tests;

pub fn ensure_routine_execute_privilege_with_control(
    authority: &dyn RoutineExecutionAuthority,
    definition: &CreateFunction,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let name = crate::routines::routine_local_name_with_control(&definition.name, control)?;
    ensure_routine_execute_privilege_named_with_control(authority, definition, &name, control)
}

pub fn ensure_routine_execute_privilege_named_with_control(
    authority: &dyn RoutineExecutionAuthority,
    definition: &CreateFunction,
    display_name: &str,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    let allowed = object_acl::try_privilege_allowed(
        &bound_routine_owner(definition)?,
        definition.execute_acl.as_deref(),
        false,
        authority.current_user_is_superuser(),
        |role| authority.current_user_has_role_identity_privileges_with_control(*role, control),
    )?;
    if allowed {
        return Ok(());
    }
    let message = control.format(format_args!(
        "permission denied for {} {}",
        routine_kind(definition),
        display_name
    ))?;
    let sqlstate = control.copy_text("42501")?;
    Err(SQLError::Routine {
        sqlstate: sqlstate.into_parts().0,
        message: message.into_parts().0,
    })
}
