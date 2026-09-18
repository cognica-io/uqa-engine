//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resolve schema authorization before checking the invoking role's database privileges.
use crate::catalog::roles::RoleReference;
use crate::{
    ast::SchemaAuthorization,
    catalog::roles::{guards::RoleCatalogGuards, RoleReferenceNames},
    SQLError,
};
pub struct SchemaCreationTarget {
    pub name: String,
    pub role_owner: RoleReference,
}
pub fn schema_creation_target(
    names: &dyn RoleReferenceNames,
    roles: &dyn RoleCatalogGuards,
    current_user: &RoleReference,
    name: Option<&str>,
    authorization: Option<&SchemaAuthorization>,
) -> Result<SchemaCreationTarget, SQLError> {
    let role_owner = match authorization {
        None | Some(SchemaAuthorization::CurrentUser) => current_user.clone(),
        Some(SchemaAuthorization::SessionUser) => names.session_role(),
        Some(SchemaAuthorization::Role(role)) => role.clone().into(),
    };
    let catalog = roles.role_definitions();
    let owner = role_owner.bind(&catalog)?;
    Ok(SchemaCreationTarget {
        name: name.unwrap_or(&owner.name).to_string(),
        role_owner: RoleReference::Bound(owner.into()),
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
