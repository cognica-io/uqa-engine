//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Role, membership, and login catalog projections.

use uqa_core::Value;
use uqa_sql::ast::RoleAttribute;
use uqa_sql::{ResultRow, SQLError};

use crate::catalog::CatalogReadView;

use super::super::helpers::rows::{bool_value, int_value, row, str_value};

pub fn build_pg_authid(catalog: &CatalogReadView) -> Vec<ResultRow> {
    catalog
        .roles()
        .map(|role| {
            row([
                ("oid", int_value(role.oid)),
                ("rolname", str_value(role.name.clone())),
                ("rolsuper", bool_value(role.has(RoleAttribute::Superuser))),
                ("rolinherit", bool_value(role.has(RoleAttribute::Inherit))),
                (
                    "rolcreaterole",
                    bool_value(role.has(RoleAttribute::CreateRole)),
                ),
                ("rolcreatedb", bool_value(role.has(RoleAttribute::CreateDb))),
                ("rolcanlogin", bool_value(role.has(RoleAttribute::Login))),
                (
                    "rolreplication",
                    bool_value(role.has(RoleAttribute::Replication)),
                ),
                ("rolconnlimit", int_value(i64::from(role.connection_limit))),
                ("rolpassword", Value::Null),
                ("rolvaliduntil", Value::Null),
                (
                    "rolbypassrls",
                    bool_value(role.has(RoleAttribute::BypassRls)),
                ),
            ])
        })
        .collect()
}

pub fn build_pg_roles(catalog: &CatalogReadView) -> Vec<ResultRow> {
    build_pg_authid(catalog)
        .into_iter()
        .map(|mut role| {
            role.insert("rolpassword".into(), str_value("********"));
            role.insert("rolconfig".into(), Value::Null);
            role
        })
        .collect()
}

pub fn build_pg_auth_members(catalog: &CatalogReadView) -> Result<Vec<ResultRow>, SQLError> {
    catalog
        .role_memberships()
        .map(|membership| {
            Ok(row([
                ("oid", int_value(membership.oid)),
                ("roleid", int_value(i64::from(membership.role.oid))),
                ("member", int_value(i64::from(membership.member.oid))),
                ("grantor", int_value(i64::from(membership.grantor.oid))),
                ("admin_option", bool_value(membership.admin_option)),
                ("inherit_option", bool_value(membership.inherit_option)),
                ("set_option", bool_value(membership.set_option)),
            ]))
        })
        .collect()
}

pub fn build_pg_user(catalog: &CatalogReadView) -> Vec<ResultRow> {
    catalog
        .roles()
        .filter(|role| role.has(RoleAttribute::Login))
        .map(|role| {
            row([
                ("usename", str_value(role.name.clone())),
                ("usesysid", int_value(role.oid)),
                ("usecreatedb", bool_value(role.has(RoleAttribute::CreateDb))),
                ("usesuper", bool_value(role.has(RoleAttribute::Superuser))),
                ("userepl", bool_value(role.has(RoleAttribute::Replication))),
                (
                    "usebypassrls",
                    bool_value(role.has(RoleAttribute::BypassRls)),
                ),
                ("passwd", str_value("********")),
                ("valuntil", Value::Null),
                ("useconfig", Value::Null),
            ])
        })
        .collect()
}

#[cfg(test)]
mod tests;
