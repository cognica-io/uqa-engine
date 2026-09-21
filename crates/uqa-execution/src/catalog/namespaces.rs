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
use uqa_sql::SQLError;

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
) -> Result<bool, SQLError> {
    if catalog.schema_security(name).is_none()
        && !uqa_sql::catalog::is_virtual_system_schema(name)
        && name != resolution.temporary_schema
    {
        catalog.observe_graph_name(name)?;
    }
    let Some(security) = schema_security(catalog, &resolution.temporary_schema, name) else {
        return Ok(false);
    };
    let definitions = &catalog.snapshot().definitions;
    Ok(security.resolve(&definitions.roles).is_ok_and(|security| {
        role_has_schema_privilege(
            &security,
            role,
            SchemaAclPrivilege::Usage,
            &definitions.roles,
            &definitions.role_memberships,
        )
    }))
}

pub fn current_schema_name(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    role: &(impl RoleSubject + ?Sized),
) -> Result<Option<String>, SQLError> {
    for name in resolution.search_path() {
        if usable_namespace(catalog, resolution, role, name)? {
            return Ok(Some(name.clone()));
        }
    }
    Ok(None)
}

pub fn current_schema_names(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    role: &(impl RoleSubject + ?Sized),
    include_implicit: bool,
) -> Result<Vec<String>, SQLError> {
    let path = resolution.search_path();
    let mut out = Vec::new();
    if include_implicit && !path.iter().any(|name| name == "pg_catalog") {
        out.push("pg_catalog".to_owned());
    }
    for name in path {
        if !out.contains(name) && usable_namespace(catalog, resolution, role, name)? {
            out.push(name.clone());
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests;
