//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Effective search-path selection over the caller's retained catalog and role.

use super::{CatalogReadView, RelationNameResolution};
use uqa_sql::catalog::{
    roles::identity::RoleSubject,
    security::{
        schema::{role_has_schema_privilege, SchemaAclPrivilege},
        BoundSchemaSecurity,
    },
};

pub(super) fn schema_security(
    catalog: &CatalogReadView,
    temporary_schema: &str,
    name: &str,
) -> Option<BoundSchemaSecurity> {
    if let Some(security) = catalog.schema_security(name) {
        return Some(security.clone());
    }
    match name {
        "pg_catalog" | "information_schema" => {
            Some(BoundSchemaSecurity::with_public_privileges(false))
        }
        "ag_catalog" => Some(BoundSchemaSecurity::bootstrap("ag_catalog")),
        name if name == temporary_schema => Some(BoundSchemaSecurity::with_public_privileges(true)),
        name if catalog.snapshot().definitions.graphs.contains_key(name) => {
            Some(BoundSchemaSecurity::bootstrap(name))
        }
        _ => None,
    }
}

fn usable_namespace(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    role: &(impl RoleSubject + ?Sized),
    name: &str,
) -> bool {
    let Some(security) = schema_security(catalog, &resolution.temporary_schema, name) else {
        return false;
    };
    let definitions = &catalog.snapshot().definitions;
    security.resolve(&definitions.roles).is_ok_and(|security| {
        role_has_schema_privilege(
            &security,
            role,
            SchemaAclPrivilege::Usage,
            &definitions.roles,
            &definitions.role_memberships,
        )
    })
}

pub fn current_schema_name(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    role: &(impl RoleSubject + ?Sized),
) -> Option<String> {
    for name in resolution.search_path() {
        if usable_namespace(catalog, resolution, role, name) {
            return Some(name.clone());
        }
    }
    None
}

pub fn current_schema_names(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    role: &(impl RoleSubject + ?Sized),
    include_implicit: bool,
) -> Vec<String> {
    let path = resolution.search_path();
    let mut out = Vec::new();
    if include_implicit && !path.iter().any(|name| name == "pg_catalog") {
        out.push("pg_catalog".to_owned());
    }
    for name in path {
        if !out.contains(name) && usable_namespace(catalog, resolution, role, name) {
            out.push(name.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests;
