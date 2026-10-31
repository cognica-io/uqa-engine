//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Namespace selection borrows retained catalog names and admits only its result and scratch.

use super::{CatalogReadView, RelationNameResolution, RoleSubject, SchemaAclPrivilege};
use uqa_core::{
    catalog_role::BoundAclEntry,
    catalog_schema::SchemaPrivileges,
    memory::{Produced, ProductionControl, ProductionVec},
};
use uqa_sql::{
    catalog::{
        roles::RoleIdentity,
        security::schema::{role_has_schema_privilege_with_control, SchemaPrivilegeView},
    },
    SQLError,
};

pub fn namespace_schema_exists(
    catalog: &CatalogReadView,
    temporary_schema: &str,
    schema: &str,
) -> bool {
    catalog.schema_security(schema).is_some()
        || (!temporary_schema.is_empty() && schema == temporary_schema)
        || temporary_toast_matches(temporary_schema, schema)
        || catalog.contains_graph(schema)
        || matches!(schema, "pg_catalog" | "information_schema" | "ag_catalog")
}

fn temporary_toast_matches(temporary_schema: &str, schema: &str) -> bool {
    !temporary_schema.is_empty()
        && schema.strip_prefix("pg_toast_temp_")
            == Some(
                temporary_schema
                    .strip_prefix("pg_temp_")
                    .unwrap_or(temporary_schema),
            )
}

fn schema_privilege(
    catalog: &CatalogReadView,
    temporary_schema: &str,
    schema: &str,
    role: &(impl RoleSubject + ?Sized),
    privilege: SchemaAclPrivilege,
    include_temporary_toast: bool,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    control.check()?;
    let definitions = &catalog.snapshot().definitions;
    let owner = RoleIdentity::BOOTSTRAP;
    let public = [
        BoundAclEntry {
            role: Some(owner),
            grantor: owner,
            privileges: SchemaPrivileges::ALL,
            grant_options: SchemaPrivileges::default(),
        },
        BoundAclEntry {
            role: None,
            grantor: owner,
            privileges: SchemaPrivileges {
                usage: true,
                create: schema == temporary_schema,
            },
            grant_options: SchemaPrivileges::default(),
        },
    ];
    let security = if let Some(security) = catalog.schema_security(schema) {
        SchemaPrivilegeView::from(security)
    } else if (!temporary_schema.is_empty() && schema == temporary_schema)
        || if catalog.contains_graph(schema) {
            schema == "public"
        } else {
            matches!(schema, "pg_catalog" | "information_schema")
        }
    {
        SchemaPrivilegeView {
            role_owner: owner,
            acl: Some(&public),
        }
    } else if schema == "ag_catalog"
        || catalog.contains_graph(schema)
        || (include_temporary_toast && temporary_toast_matches(temporary_schema, schema))
    {
        SchemaPrivilegeView {
            role_owner: owner,
            acl: None,
        }
    } else {
        return Ok(false);
    };
    role_has_schema_privilege_with_control(
        security,
        role,
        privilege,
        &definitions.roles,
        &definitions.role_memberships,
        control,
    )
}

/// Native routine namespace inquiry denies invalid persisted ACLs while propagating resource failures. The empty temporary name denotes a session whose temporary namespace has not been allocated.
pub fn schema_has_privilege_with_control(
    catalog: &CatalogReadView,
    temporary_schema: &str,
    schema: &str,
    role: &(impl RoleSubject + ?Sized),
    privilege: SchemaAclPrivilege,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    match schema_privilege(
        catalog,
        temporary_schema,
        schema,
        role,
        privilege,
        true,
        control,
    ) {
        Err(SQLError::Internal(_)) => Ok(false),
        result => result,
    }
}

pub fn require_schema_privilege_with_control(
    catalog: &CatalogReadView,
    temporary_schema: &str,
    schema: &str,
    role: &(impl RoleSubject + ?Sized),
    privilege: SchemaAclPrivilege,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    if schema_privilege(
        catalog,
        temporary_schema,
        schema,
        role,
        privilege,
        false,
        control,
    )? {
        return Ok(());
    }
    Err(SQLError::Routine {
        sqlstate: "42501".into(),
        message: format!("permission denied for schema {schema}"),
    })
}

fn contains(
    names: &[&str],
    candidate: &str,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    for name in names {
        control.check()?;
        if name.len() != candidate.len() {
            continue;
        }
        let mut equal = true;
        for (left, right) in name
            .as_bytes()
            .chunks(4096)
            .zip(candidate.as_bytes().chunks(4096))
        {
            control.check()?;
            if left != right {
                equal = false;
                break;
            }
        }
        if equal {
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn current_schema_names_with_control(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    role: &(impl RoleSubject + ?Sized),
    include_implicit: bool,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<String>>, SQLError> {
    control.check()?;
    let mut explicit = ProductionVec::new(*control);
    for name in resolution.search_path() {
        control.check()?;
        let name = if name == "pg_temp" {
            &resolution.temporary_schema
        } else {
            name
        };
        if (name == &resolution.temporary_schema && !resolution.temporary_namespace_allocated)
            || contains(&explicit, name, control)?
        {
            continue;
        }
        let usable = match schema_privilege(
            catalog,
            &resolution.temporary_schema,
            name,
            role,
            SchemaAclPrivilege::Usage,
            false,
            control,
        ) {
            // Ordinary namespace selection ignores invalid persisted ACL references, while explicit privilege checks report their original diagnostic.
            Err(SQLError::Internal(_)) => false,
            result => result?,
        };
        if usable {
            explicit.push_copy(name.as_str())?;
        }
    }
    let mut names = ProductionVec::new(*control);
    if include_implicit {
        if resolution.temporary_namespace_allocated
            && !contains(&explicit, &resolution.temporary_schema, control)?
        {
            names.push_produced(control.copy_text(&resolution.temporary_schema)?)?;
        }
        if !contains(&explicit, "pg_catalog", control)? {
            names.push_produced(control.copy_text("pg_catalog")?)?;
        }
    }
    for name in explicit.iter() {
        names.push_produced(control.copy_text(name)?)?;
    }
    Ok(names.finish()?)
}

#[cfg(test)]
mod tests;
