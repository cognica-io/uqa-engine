//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resolved privilege names and paths are admitted before construction in their original order.

use super::{role_bindings, BoundTableAclEntry, RoleDefinitions, RoleIdentity, TableSecurity};
use crate::{catalog::security::TableAclEntry, plan::copy::SyntaxProduction, SQLError};
use uqa_core::{
    catalog_acl::AclGrantee,
    memory::{OwnedMap, Produced, ProductionControl},
};

pub(super) fn resolve<'a>(
    owner: RoleIdentity,
    acl: Option<&[BoundTableAclEntry]>,
    columns: impl Iterator<Item = (&'a String, &'a Vec<BoundTableAclEntry>)>,
    roles: &RoleDefinitions,
    control: &ProductionControl<'_>,
) -> Result<Produced<TableSecurity>, SQLError> {
    let output = SyntaxProduction::new(*control);
    let name = |identity| {
        let role = role_bindings::role_definition_with_control(roles, identity, "table", control)?;
        output.text(&role.name)
    };
    let entries = |entries: &[BoundTableAclEntry]| -> Result<_, SQLError> {
        let mut values = output.vector(entries.len())?;
        for entry in entries {
            output.check()?;
            values.push(TableAclEntry {
                role: entry
                    .role
                    .map(name)
                    .transpose()?
                    .map_or(AclGrantee::Public, AclGrantee::Role),
                grantor: Some(name(entry.grantor)?),
                privileges: entry.privileges,
                grant_options: entry.grant_options,
            })?;
        }
        values.finish()
    };
    let mut result = TableSecurity {
        role_owner: name(owner)?,
        acl: acl.map(entries).transpose()?,
        column_acls: OwnedMap::new(),
    };
    for (column, acl) in columns {
        let name = output.text(column)?;
        let entries = entries(acl)?;
        let memory = control.reserve(OwnedMap::<String, Vec<TableAclEntry>>::entry_bytes())?;
        output.take(control.finish((), memory)?);
        result.column_acls.insert(name, entries);
    }
    output.finish(result)
}

pub(super) fn diagnostic(error: SQLError) -> String {
    match error {
        SQLError::Internal(message) => message,
        error => error.to_string(),
    }
}
