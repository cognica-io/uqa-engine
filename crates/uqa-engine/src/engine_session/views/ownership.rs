//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Regular-view and materialized-view ownership policy.

use super::{Engine, RelationIdentity, SQLError, StoredView, StoredViewKind};
use crate::engine_roles::role_can_set;
use crate::engine_schema_security::SchemaAclPrivilege;
use crate::engine_table_security::{role_has_table_privilege, TableAclPrivilege};

fn view_kind_name(view: &StoredView) -> &'static str {
    match view.kind {
        StoredViewKind::View => "view",
        StoredViewKind::Materialized => "materialized view",
    }
}

impl Engine {
    pub(super) fn ensure_view_owner(
        &self,
        canonical_name: &str,
        view: &StoredView,
    ) -> Result<String, SQLError> {
        if self.current_user_has_role_privileges(&view.role_owner) {
            return Ok(view.role_owner.clone());
        }
        let relation = RelationIdentity::from_legacy_name(canonical_name).map_err(|error| {
            SQLError::Internal(format!("resolve view `{canonical_name}`: {error}"))
        })?;
        Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: format!(
                "must be owner of {} {}",
                view_kind_name(view),
                relation.name
            ),
        })
    }

    pub(super) fn ensure_view_drop_authority(
        &self,
        canonical_name: &str,
        view: &StoredView,
    ) -> Result<(), SQLError> {
        let relation = RelationIdentity::from_legacy_name(canonical_name).map_err(|error| {
            SQLError::Internal(format!("resolve view `{canonical_name}`: {error}"))
        })?;
        if self.current_user_has_role_privileges(&view.role_owner)
            || self
                .schema_security_for_privilege(&relation.schema)
                .is_some_and(|security| self.current_user_has_role_privileges(&security.role_owner))
        {
            return Ok(());
        }
        Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: format!(
                "must be owner of {} {}",
                view_kind_name(view),
                relation.name
            ),
        })
    }

    pub(super) fn ensure_materialized_view_maintenance(
        &self,
        canonical_name: &str,
        view: &StoredView,
    ) -> Result<(), SQLError> {
        debug_assert_eq!(view.kind, StoredViewKind::Materialized);
        let current_user = self.current_user_name();
        let roles = self.durable.roles.read();
        let memberships = self.durable.role_memberships.read();
        if role_has_table_privilege(
            &view.security(),
            &current_user,
            TableAclPrivilege::Maintain,
            &roles,
            &memberships,
        ) {
            return Ok(());
        }
        let relation = RelationIdentity::from_legacy_name(canonical_name).map_err(|error| {
            SQLError::Internal(format!(
                "resolve materialized view `{canonical_name}`: {error}"
            ))
        })?;
        Err(SQLError::Routine {
            sqlstate: "42501".into(),
            message: format!("permission denied for materialized view {}", relation.name),
        })
    }

    pub(super) fn alter_view_role_owner(
        &self,
        canonical_name: &str,
        view: &mut StoredView,
        requested_owner: &str,
    ) -> Result<(), SQLError> {
        let current_owner = self.ensure_view_owner(canonical_name, view)?;
        let new_owner = self.resolve_role_reference(requested_owner);
        let current_user_is_superuser;
        {
            let roles = self.durable.roles.read();
            if !roles.contains_key(&new_owner) {
                return Err(SQLError::Routine {
                    sqlstate: "42704".into(),
                    message: format!("role \"{new_owner}\" does not exist"),
                });
            }
            let memberships = self.durable.role_memberships.read();
            let current_user = self.current_user_name();
            current_user_is_superuser = roles
                .get(&current_user)
                .is_some_and(|role| role.has(uqa_sql::ast::RoleAttribute::Superuser));
            if !role_can_set(&roles, &memberships, &current_user, &new_owner) {
                return Err(SQLError::Routine {
                    sqlstate: "42501".into(),
                    message: format!("must be able to SET ROLE \"{new_owner}\""),
                });
            }
        }
        if current_owner == new_owner {
            return Ok(());
        }
        let relation = RelationIdentity::from_legacy_name(canonical_name).map_err(|error| {
            SQLError::Internal(format!(
                "resolve view owner target `{canonical_name}`: {error}"
            ))
        })?;
        if !current_user_is_superuser {
            self.require_schema_privilege(
                &relation.schema,
                &new_owner,
                SchemaAclPrivilege::Create,
            )?;
        }
        let mut security = view.security();
        crate::engine_table_security::rewrite_acl_owner(&mut security, &new_owner);
        let output_columns = view.output_columns.as_deref().ok_or_else(|| {
            SQLError::Internal(format!(
                "loaded view `{canonical_name}` has no durable public column metadata"
            ))
        })?;
        crate::engine_table_security::validate_table_security_invariants(
            &security,
            Some(output_columns),
            &self.durable.roles.read(),
        )
        .map_err(|error| {
            SQLError::Internal(format!(
                "view `{canonical_name}` produced invalid privilege metadata after owner transfer: {error}"
            ))
        })?;
        view.set_security(security);
        Ok(())
    }
}
