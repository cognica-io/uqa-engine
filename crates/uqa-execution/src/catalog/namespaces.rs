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
        name if name == temporary_schema => Some(BoundSchemaSecurity::with_public_privileges(true)),
        name if catalog.snapshot().definitions.graphs.contains_key(name) => {
            Some(BoundSchemaSecurity::bootstrap(name))
        }
        _ => BoundSchemaSecurity::builtin(name),
    }
}

fn usable_namespace(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    role: &(impl RoleSubject + ?Sized),
    name: &str,
) -> bool {
    if name == resolution.temporary_schema && !resolution.temporary_namespace_allocated {
        return false;
    }
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

/// The search path's schemas that exist and that the role may use, once each, with `pg_temp` standing for the session's temporary namespace once it exists, as `recomputeNamespacePath` lists them.
fn explicit_schema_names(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    role: &(impl RoleSubject + ?Sized),
) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for name in resolution.search_path() {
        let name = if name == "pg_temp" {
            &resolution.temporary_schema
        } else {
            name
        };
        if !names.contains(name) && usable_namespace(catalog, resolution, role, name) {
            names.push(name.clone());
        }
    }
    names
}

/// `current_schema()`: the first schema of the explicit search path.
pub fn current_schema_name(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    role: &(impl RoleSubject + ?Sized),
) -> Option<String> {
    explicit_schema_names(catalog, resolution, role)
        .into_iter()
        .next()
}

/// `current_schemas(include_implicit)`: the explicit search path, which the implicitly searched schemas precede when asked for: the session's temporary namespace once it exists and then `pg_catalog`, each unless the path names it.
pub fn current_schema_names(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    role: &(impl RoleSubject + ?Sized),
    include_implicit: bool,
) -> Vec<String> {
    let explicit = explicit_schema_names(catalog, resolution, role);
    let mut out = Vec::new();
    if include_implicit {
        if resolution.temporary_namespace_allocated
            && !explicit.contains(&resolution.temporary_schema)
        {
            out.push(resolution.temporary_schema.clone());
        }
        if !explicit.iter().any(|name| name == "pg_catalog") {
            out.push("pg_catalog".to_owned());
        }
    }
    out.extend(explicit);
    out
}

#[cfg(test)]
mod tests;
