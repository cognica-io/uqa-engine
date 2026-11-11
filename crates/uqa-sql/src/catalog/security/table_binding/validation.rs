//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored authority retains the original tuple, role-resolution and grant-path validation order.

use super::{resolution, BoundTableSecurity, RoleDefinitions};
use crate::SQLError;
use uqa_core::memory::ProductionControl;

impl BoundTableSecurity {
    /// Validate the complete stored authority with admitted name projection and borrowed grant-closure scratch. Original and invoking cancellation remain active through every role, column and grant-path visit.
    pub fn validate_with_control<'a>(
        &self,
        columns: Option<impl Iterator<Item = &'a str> + Clone>,
        roles: &RoleDefinitions,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        control.check()?;
        if self.acl_revisions.relation == Some([0; 16]) {
            return Err(invalid_revision());
        }
        for (column, revision) in &self.acl_revisions.columns {
            control.check()?;
            if *revision == [0; 16] {
                return Err(invalid_revision());
            }
            let exists = if let Some(columns) = &columns {
                let mut exists = false;
                for candidate in columns.clone() {
                    control.check()?;
                    if candidate == column.as_str() {
                        exists = true;
                        break;
                    }
                }
                exists
            } else {
                true
            };
            if !exists {
                return Err(invalid_revision());
            }
        }
        let resolved = resolution::resolve(
            self.role_owner,
            self.acl.as_deref(),
            self.column_acls.iter(),
            roles,
            control,
        )?;
        crate::catalog::security::table::invariants::validate_with_control(
            &resolved, columns, roles, control,
        )
    }
}

fn invalid_revision() -> SQLError {
    SQLError::Internal("invalid relation or attribute ACL tuple identity".into())
}
