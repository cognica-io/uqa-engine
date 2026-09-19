//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read-only authority shared by object ownership checks.
use super::BoundSchemaSecurity;
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
    fn schema_security(&self, schema: &str) -> Option<BoundSchemaSecurity>;
}

pub fn require_relation_ownership(
    name: &str,
    kind: &str,
    has_owner_privileges: bool,
) -> Result<(), SQLError> {
    if has_owner_privileges {
        Ok(())
    } else {
        Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: format!("must be owner of {kind} {name}"),
        })
    }
}

/// `PostgreSQL` protects pinned catalog relations and TOAST relations after checking ownership. Unpinned catalog views retain ordinary relation validation.
pub fn reject_system_relation_alter(relation: &uqa_core::RelationIdentity) -> Result<(), SQLError> {
    let pinned = crate::catalog::SystemRelation::at(&relation.schema, &relation.name)
        .is_some_and(|system| system.oid() < 12_000);
    if pinned || relation.schema == "pg_toast" {
        Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: format!(
                "permission denied: \"{}\" is a system catalog",
                relation.name
            ),
        })
    } else {
        Ok(())
    }
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
        require_relation_ownership(
            name,
            kind,
            role_inherits(self.roles, self.memberships, self.current_user, owner),
        )?;
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
            &security.resolve(self.roles).map_err(SQLError::Internal)?,
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
