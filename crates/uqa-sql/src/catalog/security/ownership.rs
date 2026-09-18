//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read-only authority shared by object ownership checks.
use super::SchemaSecurity;
use crate::catalog::roles::identity::RoleSubject;
use crate::{
    ast::RoleAttribute,
    catalog::roles::{
        require_set_role, role_inherits, RoleDefinition, RoleMembership, RoleMembershipKey,
    },
    SQLError,
};
use std::collections::BTreeMap;

pub trait RelationOwnerSchemas {
    fn schema_security(&self, schema: &str) -> Option<SchemaSecurity>;
}

#[cfg(test)]
mod tests;

pub struct OwnerChangeAuthority<'a> {
    pub roles: &'a BTreeMap<String, RoleDefinition>,
    pub memberships: &'a BTreeMap<RoleMembershipKey, RoleMembership>,
    pub current_user: &'a dyn RoleSubject,
    pub new_owner: &'a str,
}

impl OwnerChangeAuthority<'_> {
    pub fn is_superuser(&self) -> bool {
        self.current_user
            .role_definition(self.roles)
            .is_some_and(|role| role.has(RoleAttribute::Superuser))
    }

    pub fn require_owner_change(
        &self,
        owner: &str,
        kind: &str,
        name: &str,
    ) -> Result<(), SQLError> {
        if !role_inherits(self.roles, self.memberships, self.current_user, owner) {
            return Err(SQLError::Routine {
                sqlstate: "42501".into(),
                message: format!("must be owner of {kind} {name}"),
            });
        }
        require_set_role(
            self.roles,
            self.memberships,
            self.current_user,
            self.new_owner,
        )
    }

    pub fn require_schema_create(
        &self,
        schemas: &dyn RelationOwnerSchemas,
        schema: &str,
    ) -> Result<(), SQLError> {
        if self.is_superuser() {
            return Ok(());
        }
        let security = schemas
            .schema_security(schema)
            .ok_or_else(|| SQLError::Routine {
                sqlstate: "3F000".into(),
                message: format!("schema \"{schema}\" does not exist"),
            })?;
        if super::schema::role_has_schema_privilege(
            &security,
            self.new_owner,
            super::schema::SchemaAclPrivilege::Create,
            self.roles,
            self.memberships,
        ) {
            Ok(())
        } else {
            Err(SQLError::Routine {
                sqlstate: "42501".into(),
                message: format!("permission denied for schema {schema}"),
            })
        }
    }

    pub fn require_database_create(
        &self,
        security: &super::database::BoundDatabaseSecurity,
    ) -> Result<(), SQLError> {
        if super::database::role_has_database_privilege(
            &security.resolve(self.roles).map_err(SQLError::Internal)?,
            self.current_user,
            super::database::DatabaseAclPrivilege::Create,
            self.roles,
            self.memberships,
        ) {
            Ok(())
        } else {
            Err(SQLError::Routine {
                sqlstate: "42501".into(),
                message: format!(
                    "permission denied for database {}",
                    crate::catalog::DATABASE_NAME
                ),
            })
        }
    }
}
