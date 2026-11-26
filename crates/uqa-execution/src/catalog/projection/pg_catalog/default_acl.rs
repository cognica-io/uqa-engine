//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::catalog::{
    projection::helpers::{
        acl::acl_identifier,
        rows::{catalog_array, row, str_value},
    },
    CatalogReadView,
};
use uqa_core::{catalog_role::RoleIdentity, Value};
use uqa_sql::{catalog::roles::identity::RoleSubject, ResultRow, SQLError};

pub fn rows(catalog: &CatalogReadView) -> Result<Vec<ResultRow>, SQLError> {
    let defs = &catalog.snapshot().definitions;
    let name = |role: RoleIdentity| {
        role.role_name(&defs.roles)
            .map(acl_identifier)
            .ok_or_else(|| SQLError::Internal("default ACL references a missing role".into()))
    };
    defs.default_acls
        .iter()
        .map(|(key, value)| {
            let acl = value
                .acl
                .iter()
                .map(|entry| {
                    let grantee = entry.role.map(name).transpose()?.unwrap_or_default();
                    Ok(str_value(format!(
                        "{grantee}={}/{}",
                        entry.privileges.letters(entry.grant_options),
                        name(key.owner)?
                    )))
                })
                .collect::<Result<Vec<_>, SQLError>>()?;
            Ok(row([
                ("oid", Value::Int(i64::from(value.oid))),
                ("defaclrole", Value::Int(key.owner.oid)),
                ("defaclnamespace", Value::Int(i64::from(key.namespace))),
                ("defaclobjtype", str_value(key.object.code())),
                ("defaclacl", catalog_array(acl, "default ACL")?),
            ]))
        })
        .collect()
}
