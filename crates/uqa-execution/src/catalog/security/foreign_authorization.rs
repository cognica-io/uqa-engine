//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Foreign-table authority and durable security persistence.
use crate::schema::foreign_table_alteration::{
    bound_foreign_table_security, ForeignTableAlterCatalog,
};
use uqa_core::RelationIdentity;
use uqa_sql::{
    catalog::{
        roles::{guards::RoleCatalogGuards, role_inherits, RoleReferenceNames},
        security::{
            table::{role_has_table_privilege, TableAclPrivilege},
            view_ownership::ViewOwnerSchemas,
            TableSecurity,
        },
    },
    SQLError,
};
use uqa_storage::CatalogFacade;
pub struct ForeignAuthorizationContext<'a> {
    pub names: &'a dyn RoleReferenceNames,
    pub roles: &'a dyn RoleCatalogGuards,
    pub schemas: &'a dyn ViewOwnerSchemas,
    pub catalog: &'a dyn ForeignTableAlterCatalog,
}
impl ForeignAuthorizationContext<'_> {
    pub fn ensure_foreign_table_owner(&self, name: &str) -> Result<String, SQLError> {
        let (relation, security) = bound_foreign_table_security(self.catalog, name)?;
        if self.current_user_has_role_privileges(&security.role_owner) {
            return Ok(security.role_owner);
        }
        Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: format!("must be owner of foreign table {}", relation.name),
        })
    }
    pub fn ensure_foreign_table_privilege(
        &self,
        name: &str,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        let (relation, security) = bound_foreign_table_security(self.catalog, name)?;
        let roles = self.roles.role_definitions();
        let memberships = self.roles.role_memberships();
        if role_has_table_privilege(
            &security,
            &self.names.current_user_name(),
            privilege,
            &roles,
            &memberships,
        ) {
            return Ok(());
        }
        Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: format!("permission denied for foreign table {}", relation.name),
        })
    }
    pub fn ensure_foreign_table_drop_authority(&self, name: &str) -> Result<(), SQLError> {
        let (relation, security) = bound_foreign_table_security(self.catalog, name)?;
        if self.current_user_has_role_privileges(&security.role_owner)
            || self
                .schemas
                .schema_security(&relation.schema)
                .is_some_and(|schema| self.current_user_has_role_privileges(&schema.role_owner))
        {
            return Ok(());
        }
        Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: format!("must be owner of foreign table {}", relation.name),
        })
    }
    fn current_user_has_role_privileges(&self, target: &str) -> bool {
        let current = self.names.current_user_name();
        let roles = self.roles.role_definitions();
        let memberships = self.roles.role_memberships();
        role_inherits(&roles, &memberships, &current, target)
    }
}
pub fn persist_foreign_table_security(
    catalog: Option<&dyn CatalogFacade>,
    relation: &RelationIdentity,
    security: &TableSecurity,
) -> Result<(), SQLError> {
    let Some(catalog) = catalog else {
        return Ok(());
    };
    let updated = catalog
        .update_foreign_table_security(
            relation,
            &security.role_owner,
            security.acl.as_deref(),
            &security.column_acls,
        )
        .map_err(|error| {
            SQLError::Internal(format!(
                "persist foreign table security for `{}`: {error}",
                relation.qualified_name()
            ))
        })?;
    if !updated {
        return Err(SQLError::Internal(format!(
            "foreign table `{}` disappeared from durable catalog before security update",
            relation.qualified_name()
        )));
    }
    Ok(())
}
