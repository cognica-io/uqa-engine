//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Role, membership, and login catalog projections.

use uqa_core::Value;
use uqa_sql::ast::RoleAttribute;
use uqa_sql::ResultRow;

use crate::engine_capabilities::CatalogReadView;
use crate::engine_roles::role_oid;

use super::super::helpers::rows::{bool_value, int_value, row, str_value};

pub(in crate::sql::catalog) fn build_pg_roles(catalog: &CatalogReadView) -> Vec<ResultRow> {
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
                ("rolpassword", str_value("********")),
                ("rolvaliduntil", Value::Null),
                (
                    "rolbypassrls",
                    bool_value(role.has(RoleAttribute::BypassRls)),
                ),
                ("rolconfig", Value::Null),
            ])
        })
        .collect()
}

pub(in crate::sql::catalog) fn build_pg_auth_members(catalog: &CatalogReadView) -> Vec<ResultRow> {
    catalog
        .role_memberships()
        .map(|membership| {
            row([
                ("oid", int_value(membership.oid)),
                ("roleid", int_value(role_oid(&membership.role))),
                ("member", int_value(role_oid(&membership.member))),
                ("grantor", int_value(role_oid(&membership.grantor))),
                ("admin_option", bool_value(membership.admin_option)),
                ("inherit_option", bool_value(membership.inherit_option)),
                ("set_option", bool_value(membership.set_option)),
            ])
        })
        .collect()
}

pub(in crate::sql::catalog) fn build_pg_user(catalog: &CatalogReadView) -> Vec<ResultRow> {
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
