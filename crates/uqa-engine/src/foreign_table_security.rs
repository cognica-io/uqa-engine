//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable foreign-table ownership and access-control policy.

use super::{Engine, RelationIdentity, SQLError};
use crate::state::TableSecurity;

impl Engine {
    fn bound_foreign_table_security(
        &self,
        name: &str,
    ) -> Result<(RelationIdentity, TableSecurity), SQLError> {
        uqa_execution::schema::foreign_table_alteration::bound_foreign_table_security(self, name)
    }

    pub(crate) fn ensure_foreign_table_owner(&self, name: &str) -> Result<String, SQLError> {
        let (relation, security) = self.bound_foreign_table_security(name)?;
        if self.current_user_has_role_privileges(&security.role_owner) {
            return Ok(security.role_owner);
        }
        Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: format!("must be owner of foreign table {}", relation.name),
        })
    }

    pub(crate) fn ensure_foreign_table_privilege(
        &self,
        name: &str,
        privilege: crate::table_security::TableAclPrivilege,
    ) -> Result<(), SQLError> {
        let (relation, security) = self.bound_foreign_table_security(name)?;
        let roles = self.durable.roles.read();
        let memberships = self.durable.role_memberships.read();
        if crate::table_security::role_has_table_privilege(
            &security,
            &self.current_user_name(),
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

    pub(crate) fn ensure_foreign_table_drop_authority(&self, name: &str) -> Result<(), SQLError> {
        let (relation, security) = self.bound_foreign_table_security(name)?;
        if self.current_user_has_role_privileges(&security.role_owner)
            || self
                .schema_security_for_privilege(&relation.schema)
                .is_some_and(|schema| self.current_user_has_role_privileges(&schema.role_owner))
        {
            return Ok(());
        }
        Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: format!("must be owner of foreign table {}", relation.name),
        })
    }

    pub(crate) fn persist_foreign_table_security(
        &self,
        relation: &RelationIdentity,
        security: &TableSecurity,
    ) -> Result<(), SQLError> {
        let Some(catalog) = self.storage.catalog.as_ref() else {
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
}
