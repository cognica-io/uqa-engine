//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Named authority binds once against one retained role generation.

use super::{
    object_acl, BoundRoutineAuthority, LegacyRoutineAclEntry, RoleDefinitions, RoutineAclEntry,
    SQLError,
};
use crate::catalog::{roles::identity::RoleBinding, work::Work};
use uqa_core::{
    catalog_role::RoleIdentity,
    image_io::AsyncImageInput,
    memory::{MapKeyInput, Produced, ProductionControl, ProductionString, ProductionVec},
};

/// Bind a completely decoded legacy ACL through the enclosing input's work channel. Native ACL storage owns the original allocation allowance; no display-name binding is copied and discarded.
pub async fn bind_legacy_authority_from_input(
    owner: &str,
    acl: Option<&[LegacyRoutineAclEntry]>,
    roles: &RoleDefinitions,
    implicit_owner_execute: bool,
    control: &ProductionControl<'_>,
    input: &mut AsyncImageInput<'_>,
) -> Result<Produced<BoundRoutineAuthority>, SQLError> {
    bind_from(owner, acl, roles, implicit_owner_execute, control, input).await
}

pub(super) async fn bind_from(
    owner: &str,
    acl: Option<&[LegacyRoutineAclEntry]>,
    roles: &RoleDefinitions,
    implicit_owner_execute: bool,
    control: &ProductionControl<'_>,
    work: &mut (impl Work + Send),
) -> Result<Produced<BoundRoutineAuthority>, SQLError> {
    control.check()?;
    let identity = bind(owner, roles, control, work).await?;
    let (execute_acl, memory) = if let Some(acl) = acl {
        let mut output = ProductionVec::new(*control);
        let mut owner_present = false;
        for entry in acl {
            work.checkpoint().await?;
            control.check()?;
            let role = match entry.role.role_name() {
                Some(name) => Some(bind(name, roles, control, work).await?),
                None => None,
            };
            let grantor = match entry.grantor.as_deref() {
                Some(name) => bind(name, roles, control, work).await?,
                None => identity,
            };
            owner_present |= role == Some(identity);
            output.push_produced(control.finish(
                RoutineAclEntry {
                    role,
                    grantor,
                    grant_option: entry.grant_option,
                },
                control.empty_reservation(),
            )?)?;
        }
        if implicit_owner_execute && !owner_present {
            work.checkpoint().await?;
            output.push_produced(control.finish(
                RoutineAclEntry {
                    role: Some(identity),
                    grantor: identity,
                    grant_option: false,
                },
                control.empty_reservation(),
            )?)?;
        }
        let (output, memory) = output.finish()?.into_parts();
        (Some(output), memory)
    } else {
        (None, control.empty_reservation())
    };
    let bound = control.finish(
        BoundRoutineAuthority {
            owner: identity,
            execute_acl,
        },
        memory,
    )?;
    object_acl::validate_acl_from(
        identity,
        bound.execute_acl.as_deref(),
        "routine",
        control,
        work,
    )
    .await?;
    control.check()?;
    Ok(bound)
}

async fn bind(
    name: &str,
    roles: &RoleDefinitions,
    control: &ProductionControl<'_>,
    work: &mut (impl Work + Send),
) -> Result<RoleIdentity, SQLError> {
    if let Some(role) = roles
        .get_from(name, &mut NameInput { control, work })
        .await?
    {
        return RoleBinding::identity_from_definition(role);
    }
    let mut message = ProductionString::new(*control);
    message.reserve(
        name.len()
            .checked_add("role \"".len() + "\" does not exist".len())
            .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?,
    )?;
    message.push_str("role \"")?;
    let mut begin = 0;
    while begin < name.len() {
        work.checkpoint().await?;
        control.check()?;
        let mut end = (begin + 4096).min(name.len());
        while !name.is_char_boundary(end) {
            end -= 1;
        }
        message.push_str(&name[begin..end])?;
        begin = end;
    }
    message.push_str("\" does not exist")?;
    Err(SQLError::admit_routine_message(
        "42704",
        message.finish()?,
        control,
    )?)
}

struct NameInput<'a, 'c, W> {
    control: &'a ProductionControl<'c>,
    work: &'a mut W,
}

impl<W: Work + Send> MapKeyInput<str, String> for NameInput<'_, '_, W> {
    type Error = SQLError;

    async fn checkpoint(&mut self) -> Result<(), SQLError> {
        self.control.check()?;
        self.work.checkpoint().await?;
        Ok(())
    }

    async fn compare(
        &mut self,
        query: &str,
        stored: &String,
    ) -> Result<std::cmp::Ordering, SQLError> {
        self.checkpoint().await?;
        let length = query.len().min(stored.len());
        let mut begin = 0;
        while begin < length {
            self.checkpoint().await?;
            let end = (begin + 4096).min(length);
            let order = query.as_bytes()[begin..end].cmp(&stored.as_bytes()[begin..end]);
            if !order.is_eq() {
                return Ok(order);
            }
            begin = end;
        }
        Ok(query.len().cmp(&stored.len()))
    }
}
