//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored system authority uses the same role projection and grant-path traversal as ordinary restoration.

use super::{SystemRelation, SystemRelationSecurity, TableSecurity};
use crate::{
    catalog::{
        roles::{RoleDefinitions, RoleIdentity},
        security::{role_bindings, table, table_binding},
        work::{inline, Work},
    },
    SQLError,
};
use uqa_core::{image_io::AsyncImageInput, memory::ProductionControl};

pub fn validate_security(
    relation: SystemRelation,
    security: &TableSecurity,
    roles: &RoleDefinitions,
) -> Result<(), String> {
    inline(validate(
        relation,
        security,
        roles,
        &ProductionControl::uncontrolled(),
        &mut (),
    ))
    .map_err(diagnostic)
}

async fn validate(
    relation: SystemRelation,
    security: &TableSecurity,
    roles: &RoleDefinitions,
    control: &ProductionControl<'_>,
    work: &mut impl Work,
) -> Result<(), SQLError> {
    work.checkpoint().await?;
    control.check()?;
    if role_bindings::bind_role(roles, &security.role_owner, "system relation")
        .map_err(SQLError::Internal)?
        != RoleIdentity::BOOTSTRAP
    {
        return Err(SQLError::Internal(format!(
            "system relation `{}` has an invalid owner",
            relation.qualified_name()
        )));
    }
    let columns = relation.column_names_with_control(control)?;
    table::invariants::validate_from(
        security,
        Some(columns.iter().map(String::as_str)),
        roles,
        control,
        work,
    )
    .await
}

impl SystemRelationSecurity {
    /// Preserve complete name resolution before owner and grant validation. Empty attribute overrides remain stored but do not enter effective authority.
    pub async fn validate_from_input(
        &self,
        relation: SystemRelation,
        roles: &RoleDefinitions,
        control: &ProductionControl<'_>,
        work: &mut AsyncImageInput<'_>,
    ) -> Result<(), SQLError> {
        let (bootstrap, count) = relation.bootstrap_acl();
        let acl = self
            .table
            .as_ref()
            .map_or(&bootstrap[..count], |value| value.acl.as_slice());
        let columns = self
            .columns
            .iter()
            .filter(|(_, value)| !value.acl.is_empty())
            .map(|(name, value)| (name, &value.acl));
        let security = table_binding::resolution::resolve_from(
            RoleIdentity::BOOTSTRAP,
            Some(acl),
            columns,
            roles,
            control,
            work,
        )
        .await?;
        validate(relation, &security, roles, control, work).await
    }
}

fn diagnostic(error: SQLError) -> String {
    match error {
        SQLError::Internal(message) => message,
        error => error.to_string(),
    }
}
