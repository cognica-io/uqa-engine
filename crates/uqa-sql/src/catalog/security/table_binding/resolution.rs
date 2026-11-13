//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resolved privilege names and paths are admitted before construction in their original order.

use super::{role_bindings, BoundTableAclEntry, RoleDefinitions, RoleIdentity, TableSecurity};
use crate::{
    catalog::{
        security::TableAclEntry,
        work::{inline, Work},
    },
    SQLError,
};
use uqa_core::{
    catalog_acl::AclGrantee,
    memory::{MemoryReservation, OwnedMap, Produced, ProductionControl, ProductionVec},
};

struct Output {
    value: TableSecurity,
    memory: Option<MemoryReservation>,
}

pub(super) fn resolve<'a>(
    owner: RoleIdentity,
    acl: Option<&[BoundTableAclEntry]>,
    columns: impl Iterator<Item = (&'a String, &'a Vec<BoundTableAclEntry>)>,
    roles: &RoleDefinitions,
    control: &ProductionControl<'_>,
) -> Result<Produced<TableSecurity>, SQLError> {
    inline(resolve_from(owner, acl, columns, roles, control, &mut ()))
}

pub(in crate::catalog::security) async fn resolve_from<'a>(
    owner: RoleIdentity,
    acl: Option<&[BoundTableAclEntry]>,
    columns: impl Iterator<Item = (&'a String, &'a Vec<BoundTableAclEntry>)>,
    roles: &RoleDefinitions,
    control: &ProductionControl<'_>,
    work: &mut impl Work,
) -> Result<Produced<TableSecurity>, SQLError> {
    let (role_owner, memory) = name(owner, roles, control, work).await?.into_parts();
    let mut output = Output {
        value: TableSecurity {
            role_owner,
            acl: None,
            column_acls: OwnedMap::new(),
        },
        memory,
    };
    if let Some(acl) = acl {
        let (acl, memory) = entries(acl, roles, control, work).await?.into_parts();
        output.memory = control.combine(output.memory.take(), memory);
        output.value.acl = Some(acl);
    }
    for (column, acl) in columns {
        work.checkpoint().await?;
        control.check()?;
        let column = work.text(column, control).await?;
        let acl = entries(acl, roles, control, work).await?;
        let node = control.reserve(OwnedMap::<String, Vec<TableAclEntry>>::entry_bytes())?;
        let (column, column_memory) = column.into_parts();
        let (acl, acl_memory) = acl.into_parts();
        output.memory = control.combine(output.memory.take(), node);
        output.memory = control.combine(output.memory.take(), column_memory);
        output.memory = control.combine(output.memory.take(), acl_memory);
        output.value.column_acls.insert(column, acl);
    }
    Ok(control.finish(output.value, output.memory)?)
}

async fn name(
    identity: RoleIdentity,
    roles: &RoleDefinitions,
    control: &ProductionControl<'_>,
    work: &mut impl Work,
) -> Result<Produced<String>, SQLError> {
    let role = role_bindings::role_definition_from(roles, identity, "table", control, work).await?;
    work.text(&role.name, control).await
}

async fn entries(
    entries: &[BoundTableAclEntry],
    roles: &RoleDefinitions,
    control: &ProductionControl<'_>,
    work: &mut impl Work,
) -> Result<Produced<Vec<TableAclEntry>>, SQLError> {
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
                let (role, owned) = role.into_parts();
                memory = control.combine(memory, owned);
                AclGrantee::Role(role)
            }
            None => AclGrantee::Public,
        };
        values.push_produced(control.finish(
            TableAclEntry {
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
