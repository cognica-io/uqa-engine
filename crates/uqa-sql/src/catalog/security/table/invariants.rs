//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validate table and column ACL grant paths against durable relation metadata.
use super::{TableAclEntry, TableAclPrivilege, TableSecurity};
use crate::catalog::roles::RoleDefinitions;
use crate::SQLError;
use uqa_core::memory::{ProductionControl, ProductionMap};

pub fn validate_table_security_invariants(
    security: &TableSecurity,
    columns: Option<&[String]>,
    roles: &RoleDefinitions,
) -> Result<(), String> {
    validate_with_control(
        security,
        columns.map(|columns| columns.iter().map(String::as_str)),
        roles,
        &ProductionControl::uncontrolled(),
    )
    .map_err(|error| match error {
        SQLError::Internal(message) => message,
        error => error.to_string(),
    })
}

pub(in crate::catalog::security) fn validate_with_control<'a>(
    security: &TableSecurity,
    columns: Option<impl Iterator<Item = &'a str> + Clone>,
    roles: &RoleDefinitions,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;

    if let Some(acl) = security.acl.as_deref() {
        validate_acl(security, acl, None, roles, control)?;
    }
    if !security.column_acls.is_empty() && columns.is_none() {
        return Err(SQLError::Internal(
            "column ACLs require durable public column metadata".into(),
        ));
    }
    for (column, acl) in &security.column_acls {
        control.check()?;
        let mut exists = false;
        for candidate in columns.clone().into_iter().flatten() {
            control.check()?;
            if candidate == column.as_str() {
                exists = true;
                break;
            }
        }
        if !exists {
            return Err(SQLError::Internal(format!(
                "column ACL references missing column `{column}`"
            )));
        }
        for entry in acl {
            control.check()?;
            if entry.privileges.delete
                || entry.privileges.truncate
                || entry.privileges.trigger
                || entry.privileges.maintain
                || entry.grant_options.delete
                || entry.grant_options.truncate
                || entry.grant_options.trigger
                || entry.grant_options.maintain
            {
                return Err(SQLError::Internal(format!(
                    "column ACL for `{column}` contains a relation-only privilege"
                )));
            }
        }
        validate_acl(security, acl, Some(column), roles, control)?;
    }
    Ok(())
}

fn grant_options<'a, 'c>(
    security: &'a TableSecurity,
    column: Option<&str>,
    privilege: TableAclPrivilege,
    control: &ProductionControl<'c>,
) -> Result<ProductionMap<'c, &'a str, ()>, SQLError> {
    let mut reachable = ProductionMap::new(*control);
    reachable.insert_copy(security.role_owner.as_str(), ())?;
    super::grant_paths::extend(
        &security.role_owner,
        security.acl.as_deref(),
        privilege,
        &mut reachable,
        control,
    )?;
    if let Some(column) = column {
        super::grant_paths::extend(
            &security.role_owner,
            security.column_acls.get(column).map(Vec::as_slice),
            privilege,
            &mut reachable,
            control,
        )?;
    }
    Ok(reachable)
}

fn validate_acl(
    security: &TableSecurity,
    acl: &[TableAclEntry],
    column: Option<&str>,
    roles: &RoleDefinitions,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let mut paths = ProductionMap::new(*control);
    for entry in acl {
        control.check()?;
        let grantor = super::acl_grantor(entry, &security.role_owner);
        if entry
            .role
            .role_name()
            .is_some_and(|name| !roles.contains_key(name))
        {
            return Err(SQLError::Internal(format!(
                "ACL references missing grantee role `{}`",
                entry.role
            )));
        }
        if !roles.contains_key(grantor) {
            return Err(SQLError::Internal(format!(
                "ACL references missing grantor role `{grantor}`"
            )));
        }
        if paths.insert_copy((&entry.role, grantor), ())?.is_some() {
            return Err(SQLError::Internal(format!(
                "ACL contains duplicate grant path `{grantor}` -> `{}`",
                entry.role
            )));
        }
        if entry.privileges.is_empty() && entry.grant_options.is_empty() {
            return Err(SQLError::Internal(
                "ACL contains an empty grant path".into(),
            ));
        }
        if entry.role.is_public() && !entry.grant_options.is_empty() {
            return Err(SQLError::Internal(
                "PUBLIC cannot hold grant options".into(),
            ));
        }
        for privilege in TableAclPrivilege::ALL {
            let mask = privilege.mask();
            if entry.grant_options.intersects(mask) && !entry.privileges.intersects(mask) {
                return Err(SQLError::Internal(
                    "ACL grant option exists without its privilege".into(),
                ));
            }
            if entry.privileges.intersects(mask) || entry.grant_options.intersects(mask) {
                let reachable = grant_options(security, column, privilege, control)?;
                if reachable.get(grantor).is_none() {
                    return Err(SQLError::Internal(format!(
                        "ACL grant path from `{grantor}` is not rooted at owner `{}`",
                        security.role_owner
                    )));
                }
            }
        }
    }
    Ok(())
}
