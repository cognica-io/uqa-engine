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
        crate::catalog::work::inline(self.validate_from(columns, roles, control, &mut ()))
    }

    /// Resume role resolution, column checks and ordered grant closure through the original input work channel. The native authority is borrowed unchanged while every temporary allocation retains the supplied production control.
    pub async fn validate_from_input<'a>(
        &self,
        columns: Option<impl Iterator<Item = &'a str> + Clone>,
        roles: &RoleDefinitions,
        control: &ProductionControl<'_>,
        input: &mut uqa_core::image_io::AsyncImageInput<'_>,
    ) -> Result<(), SQLError> {
        self.validate_from(columns, roles, control, input).await
    }

    async fn validate_from<'a>(
        &self,
        columns: Option<impl Iterator<Item = &'a str> + Clone>,
        roles: &RoleDefinitions,
        control: &ProductionControl<'_>,
        work: &mut impl crate::catalog::work::Work,
    ) -> Result<(), SQLError> {
        control.check()?;
        if self.acl_revisions.relation == Some([0; 16]) {
            return Err(invalid_revision());
        }
        for (column, revision) in &self.acl_revisions.columns {
            work.checkpoint().await?;
            control.check()?;
            if *revision == [0; 16] {
                return Err(invalid_revision());
            }
            let exists = if let Some(columns) = &columns {
                let mut exists = false;
                for candidate in columns.clone() {
                    work.checkpoint().await?;
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
        let resolved = resolution::resolve_from(
            self.role_owner,
            self.acl.as_deref(),
            self.column_acls.iter(),
            roles,
            control,
            work,
        )
        .await?;
        crate::catalog::security::table::invariants::validate_from(
            &resolved, columns, roles, control, work,
        )
        .await
    }
}

fn invalid_revision() -> SQLError {
    SQLError::Internal("invalid relation or attribute ACL tuple identity".into())
}
