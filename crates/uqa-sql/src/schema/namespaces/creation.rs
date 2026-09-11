//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resolve schema authorization before checking the invoking role's database privileges.
use crate::{
    ast::SchemaAuthorization,
    catalog::roles::{guards::RoleCatalogGuards, require_role_exists, RoleReferenceNames},
    SQLError,
};
pub struct SchemaCreationTarget {
    pub name: String,
    pub role_owner: String,
}
pub fn schema_creation_target(
    names: &dyn RoleReferenceNames,
    roles: &dyn RoleCatalogGuards,
    current_user: &str,
    name: Option<&str>,
    authorization: Option<&SchemaAuthorization>,
) -> Result<SchemaCreationTarget, SQLError> {
    let role_owner = match authorization {
        None | Some(SchemaAuthorization::CurrentUser) => current_user.to_string(),
        Some(SchemaAuthorization::SessionUser) => names.session_user_name(),
        Some(SchemaAuthorization::Role(role)) => role.clone(),
    };
    if authorization.is_some() {
        require_role_exists(&roles.role_definitions(), &role_owner)?;
    }
    Ok(SchemaCreationTarget {
        name: name.unwrap_or(&role_owner).to_string(),
        role_owner,
    })
}
pub fn validate_schema_creation_name(name: &str) -> Result<(), SQLError> {
    if name.starts_with("pg_") {
        return Err(SQLError::Routine {
            sqlstate: "42939".into(),
            message: format!(r#"unacceptable schema name "{name}""#),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests;
