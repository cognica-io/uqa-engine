//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Role renaming validates names and authority without changing the selected incarnation.

use super::{
    definition::{require_role_administration_for, RoleValidationContext},
    identity::RoleSubject,
    memberships::{insufficient_privilege, role_is_superuser},
    RoleDefinition,
};
use crate::{ast::RenameRoleStmt, SQLError};
use std::collections::BTreeMap;

pub fn rename_candidate(
    context: &RoleValidationContext<'_>,
    roles: &BTreeMap<String, RoleDefinition>,
    statement: &RenameRoleStmt,
) -> Result<RoleDefinition, SQLError> {
    let existing = roles
        .get(&statement.name)
        .ok_or_else(|| SQLError::Routine {
            sqlstate: "42704".into(),
            message: format!("role \"{}\" does not exist", statement.name),
        })?;
    for (subject, description) in [
        (context.names.session_role(), "session"),
        (context.names.outer_role(), "current"),
    ] {
        if subject
            .role_definition(roles)
            .is_some_and(|role| role.identity() == existing.identity())
        {
            return Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: format!("{description} user cannot be renamed"),
            });
        }
    }
    for name in [&existing.name, &statement.new_name] {
        if name.starts_with("pg_") {
            return Err(SQLError::Routine {
                sqlstate: "42939".into(),
                message: format!("role name \"{name}\" is reserved"),
            });
        }
    }
    require_available_name(roles, &statement.new_name, false)?;
    let current = context.names.current_role();
    if existing.has(crate::ast::RoleAttribute::Superuser) && !role_is_superuser(roles, &current) {
        return Err(insufficient_privilege("permission denied to rename role"));
    }
    require_role_administration_for(
        context.roles,
        roles,
        &current,
        &existing.name,
        "rename role",
    )?;
    let mut renamed = existing.clone();
    renamed.name.clone_from(&statement.new_name);
    renamed.advance_revision()?;
    Ok(renamed)
}

/// Initial duplicates precede authorization; a destination occupied during the mutation is a uniqueness violation.
pub fn require_available_name(
    roles: &BTreeMap<String, RoleDefinition>,
    name: &str,
    publication: bool,
) -> Result<(), SQLError> {
    if roles.contains_key(name) {
        return Err(SQLError::Routine {
            sqlstate: if publication { "23505" } else { "42710" }.into(),
            message: if publication {
                "duplicate key value violates unique constraint \"pg_authid_rolname_index\"".into()
            } else {
                format!("role \"{name}\" already exists")
            },
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests;
