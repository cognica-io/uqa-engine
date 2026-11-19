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
    memory::{Produced, ProductionControl},
};
use uqa_sql::{
    catalog::{
        roles::RoleIdentity,
        security::schema::{role_has_schema_privilege_with_control, SchemaPrivilegeView},
    },
    SQLError,
};

pub(in crate::catalog) trait NamespaceWork:
    crate::catalog::selection_work::Work
{
    fn overlay_contains(
        &mut self,
        names: &std::collections::BTreeSet<String>,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> impl std::future::Future<Output = Result<bool, SQLError>> + Send;
}
impl NamespaceWork for () {
    async fn overlay_contains(
        &mut self,
        names: &std::collections::BTreeSet<String>,
        name: &str,
        _: &ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        Ok(names.contains(name))
    }
}
impl NamespaceWork for uqa_core::image_io::AsyncImageInput<'_> {
    async fn overlay_contains(
        &mut self,
        names: &std::collections::BTreeSet<String>,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        for stored in names {
            match crate::catalog::selection_work::compare(name, stored, control, self).await? {
                std::cmp::Ordering::Less => break,
                std::cmp::Ordering::Equal => return Ok(true),
                std::cmp::Ordering::Greater => (),
            }
        }
        Ok(false)
    }
}

pub fn namespace_schema_exists(
    catalog: &CatalogReadView,
    temporary_schema: &str,
    schema: &str,
) -> bool {
    crate::catalog::selection_work::inline(namespace_schema_exists_with_work(
        catalog,
        temporary_schema,
        schema,
        &ProductionControl::uncontrolled(),
        &mut (),
    ))
    .expect("uncontrolled namespace membership")
}

pub(in crate::catalog) async fn namespace_schema_exists_with_work<W: NamespaceWork>(
    catalog: &CatalogReadView,
    temporary_schema: &str,
    schema: &str,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<bool, SQLError> {
    use crate::catalog::selection_work::{checkpoint, equal, NameComparison};
    checkpoint(control, work).await?;
    let snapshot = catalog.snapshot();
    if snapshot
        .definitions
        .schemas
        .contains_key_from(
            schema,
            &mut NameComparison {
                control: *control,
                work,
            },
        )
        .await?
        || (!temporary_schema.is_empty() && equal(schema, temporary_schema, control, work).await?)
        || temporary_toast_matches(temporary_schema, schema)
        || matches!(schema, "pg_catalog" | "information_schema" | "ag_catalog")
    {
        return Ok(true);
    }
    if let Some(overlay) = &snapshot.graph_overlay {
        return work.overlay_contains(&overlay.names, schema, control).await;
    }
    snapshot
        .definitions
        .graphs
        .contains_key_from(
            schema,
            &mut NameComparison {
                control: *control,
                work,
            },
        )
        .await
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

pub(super) fn schema_privilege(
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
    let public = public_acl(schema == temporary_schema);
    let security = catalog
        .schema_security(schema)
        .map(SchemaPrivilegeView::from)
        .or_else(|| {
            fallback_security(
                schema,
                !temporary_schema.is_empty() && schema == temporary_schema,
                catalog.contains_graph(schema),
                include_temporary_toast && temporary_toast_matches(temporary_schema, schema),
                &public,
            )
        });
    let Some(security) = security else {
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

pub fn current_schema_names_with_control(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    role: &(impl RoleSubject + ?Sized),
    include_implicit: bool,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<String>>, SQLError> {
    super::input::ordinary(catalog, resolution, role, include_implicit, control)
}

pub(super) fn public_acl(temporary: bool) -> [BoundAclEntry<SchemaPrivileges>; 2] {
    let owner = RoleIdentity::BOOTSTRAP;
    [
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
                create: temporary,
            },
            grant_options: SchemaPrivileges::default(),
        },
    ]
}

pub(super) fn fallback_security<'a>(
    schema: &str,
    temporary: bool,
    graph: bool,
    toast: bool,
    public: &'a [BoundAclEntry<SchemaPrivileges>],
) -> Option<SchemaPrivilegeView<'a>> {
    if temporary
        || if graph {
            schema == "public"
        } else {
            matches!(schema, "pg_catalog" | "information_schema")
        }
    {
        Some(SchemaPrivilegeView {
            role_owner: RoleIdentity::BOOTSTRAP,
            acl: Some(public),
        })
    } else if schema == "ag_catalog" || graph || toast {
        Some(SchemaPrivilegeView {
            role_owner: RoleIdentity::BOOTSTRAP,
            acl: None,
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests;
