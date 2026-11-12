//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sequence role projection retains every original incarnation and admits resolved names before construction.

use super::{
    role_bindings, BoundSequenceAclEntry, BoundSequenceSecurity, RoleDefinitions, RoleIdentity,
    SequenceSecurity,
};
use crate::{
    catalog::{
        security::sequence::invariants,
        work::{inline, Work},
    },
    SQLError,
};
use uqa_core::{
    catalog_acl::AclGrantee,
    catalog_sequence::SequenceAclEntry,
    image_io::AsyncImageInput,
    memory::{Produced, ProductionControl, ProductionVec},
};

impl BoundSequenceSecurity {
    pub fn resolve_with_control(
        &self,
        roles: &RoleDefinitions,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<SequenceSecurity>, SQLError> {
        inline(resolve_from(
            self.role_owner,
            self.acl.as_deref(),
            roles,
            control,
            &mut (),
        ))
    }

    pub fn validate_with_control(
        &self,
        roles: &RoleDefinitions,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        inline(validate_from(
            self.role_owner,
            self.acl.as_deref(),
            roles,
            control,
            &mut (),
        ))
    }

    /// Preserve complete role projection before ordered grant validation through the original input work channel.
    pub async fn validate_from_input(
        &self,
        roles: &RoleDefinitions,
        control: &ProductionControl<'_>,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<(), SQLError> {
        validate_from(self.role_owner, self.acl.as_deref(), roles, control, input).await
    }

    /// Validate stored authority before transferring its existing ACL allocation into the native registry.
    pub async fn validate_stored_from_input(
        row: &uqa_core::catalog_sequence::BoundSequenceSecurity,
        roles: &RoleDefinitions,
        control: &ProductionControl<'_>,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<(), SQLError> {
        validate_from(row.role_owner, row.acl.as_deref(), roles, control, input).await
    }
}

async fn validate_from(
    owner: RoleIdentity,
    acl: Option<&[BoundSequenceAclEntry]>,
    roles: &RoleDefinitions,
    control: &ProductionControl<'_>,
    work: &mut impl Work,
) -> Result<(), SQLError> {
    let resolved = resolve_from(owner, acl, roles, control, work).await?;
    invariants::validate_from(&resolved, roles, control, work).await
}

async fn resolve_from(
    owner: RoleIdentity,
    acl: Option<&[BoundSequenceAclEntry]>,
    roles: &RoleDefinitions,
    control: &ProductionControl<'_>,
    work: &mut impl Work,
) -> Result<Produced<SequenceSecurity>, SQLError> {
    let owner = name(owner, roles, control, work).await?;
    let acl = if let Some(entries) = acl {
        Some(entries_from(entries, roles, control, work).await?)
    } else {
        None
    };
    let (role_owner, mut memory) = owner.into_parts();
    let acl = match acl {
        Some(acl) => {
            let (acl, lease) = acl.into_parts();
            memory = control.combine(memory, lease);
            Some(acl)
        }
        None => None,
    };
    Ok(control.finish(SequenceSecurity { role_owner, acl }, memory)?)
}

async fn name(
    identity: RoleIdentity,
    roles: &RoleDefinitions,
    control: &ProductionControl<'_>,
    work: &mut impl Work,
) -> Result<Produced<String>, SQLError> {
    let role =
        role_bindings::role_definition_from(roles, identity, "sequence", control, work).await?;
    work.text(&role.name, control).await
}

async fn entries_from(
    entries: &[BoundSequenceAclEntry],
    roles: &RoleDefinitions,
    control: &ProductionControl<'_>,
    work: &mut impl Work,
) -> Result<Produced<Vec<SequenceAclEntry>>, SQLError> {
    let mut values = ProductionVec::new(*control);
    values.reserve(entries.len())?;
    for entry in entries {
        work.checkpoint().await?;
        control.check()?;
        let role = match entry.role {
            Some(identity) => Some(name(identity, roles, control, work).await?),
            None => None,
        };
        let grantor = name(entry.grantor, roles, control, work).await?;
        let (grantor, mut memory) = grantor.into_parts();
        let role = match role {
            Some(role) => {
                let (role, lease) = role.into_parts();
                memory = control.combine(memory, lease);
                AclGrantee::Role(role)
            }
            None => AclGrantee::Public,
        };
        values.push_produced(control.finish(
            SequenceAclEntry {
                role,
                grantor: Some(grantor),
                privileges: entry.privileges,
                grant_options: entry.grant_options,
            },
            memory,
        )?)?;
    }
    Ok(values.finish()?)
}

pub(super) fn diagnostic(error: SQLError) -> String {
    match error {
        SQLError::Internal(message) => message,
        error => error.to_string(),
    }
}
