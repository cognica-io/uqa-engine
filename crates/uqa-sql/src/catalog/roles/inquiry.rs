//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Role privilege inquiry with ordered session and retained catalog reads.

use super::{
    guards::RoleCatalogGuards,
    memberships::{
        parse_pg_has_role_privileges, pg_has_role_privilege, resolve_pg_has_role_identifier,
        role_privilege_text,
    },
    RoleReferenceNames,
};
use crate::SQLError;
use uqa_core::Value;

pub fn pg_has_role_value(
    names: &dyn RoleReferenceNames,
    catalog: &dyn RoleCatalogGuards,
    arguments: &[Value],
) -> Result<Value, SQLError> {
    if arguments.iter().any(|argument| argument == &Value::Null) {
        return Ok(Value::Null);
    }
    let (subject_value, target_value, privilege_value) = match arguments {
        [target, privilege] => (None, target, privilege),
        [subject, target, privilege] => (Some(subject), target, privilege),
        _ => {
            return Err(SQLError::BadArity {
                name: "pg_has_role".into(),
                expected: "2 or 3".into(),
                actual: arguments.len(),
            });
        }
    };
    let current_user = subject_value.is_none().then(|| names.current_user_name());
    let roles = catalog.role_definitions();
    let subject = subject_value.map_or_else(
        || Ok(current_user),
        |value| resolve_pg_has_role_identifier(value, &roles),
    )?;
    let target = resolve_pg_has_role_identifier(target_value, &roles)?;
    let privileges = parse_pg_has_role_privileges(role_privilege_text(privilege_value)?)?;
    let memberships = catalog.role_memberships();
    let allowed = privileges.into_iter().any(|privilege| {
        pg_has_role_privilege(
            &roles,
            &memberships,
            subject.as_deref(),
            target.as_deref(),
            privilege,
        )
    });
    Ok(Value::Bool(allowed))
}
