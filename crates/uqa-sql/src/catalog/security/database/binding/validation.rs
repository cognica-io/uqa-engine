//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Database authority resolves its original owner and ACL endpoints through one traversal.

use super::{role_bindings, BoundDatabaseSecurity, RoleDefinitions};
use crate::{catalog::work::Work, SQLError};
use uqa_core::{image_io::AsyncImageInput, memory::ProductionControl};

impl BoundDatabaseSecurity {
    pub fn validate(&self, roles: &RoleDefinitions) -> Result<(), String> {
        crate::catalog::work::inline(self.validate_from(
            roles,
            &ProductionControl::uncontrolled(),
            &mut (),
        ))
        .map_err(|error| match error {
            SQLError::Internal(message) => message,
            error => error.to_string(),
        })
    }

    /// Retain owner-before-ACL and grantee-before-grantor resolution across the enclosing input's work windows. The authority is borrowed unchanged; every role visit honors both cancellation owners.
    pub async fn validate_from_input(
        &self,
        roles: &RoleDefinitions,
        control: &ProductionControl<'_>,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<(), SQLError> {
        self.validate_from(roles, control, input).await
    }

    async fn validate_from(
        &self,
        roles: &RoleDefinitions,
        control: &ProductionControl<'_>,
        work: &mut impl Work,
    ) -> Result<(), SQLError> {
        role_bindings::role_definition_from(roles, self.role_owner, "database", control, work)
            .await?;
        for entry in self.acl.iter().flatten() {
            work.checkpoint().await?;
            control.check()?;
            if let Some(grantee) = entry.role {
                role_bindings::role_definition_from(roles, grantee, "database", control, work)
                    .await?;
            }
            role_bindings::role_definition_from(roles, entry.grantor, "database", control, work)
                .await?;
        }
        control.check()?;
        Ok(())
    }
}
