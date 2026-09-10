//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Regular-view and materialized-view ownership policy.

use super::{Engine, RelationIdentity, SQLError, StoredView, StoredViewKind};
use crate::table_security::{role_has_table_privilege, TableAclPrivilege};

fn view_kind_name(view: &StoredView) -> &'static str {
    match view.kind {
        StoredViewKind::View => "view",
        StoredViewKind::Materialized => "materialized view",
    }
}

impl Engine {
    pub(crate) fn ensure_view_owner(
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
}
