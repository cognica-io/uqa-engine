//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrow selected view inputs for the information-schema column projection.

use super::{information_schema_column_row, view_columns_for};
use crate::catalog::{
    context::CatalogContext, projection::CatalogRequest, CatalogReadView, RelationNameResolution,
};
use uqa_sql::{
    ast::ColumnDef,
    catalog::{
        security::{
            system_relations::{self, SystemRelationSecurityCatalog},
            table::{TableAclPrivilege, TablePrivilegeCheck},
        },
        SystemRelation, VirtualRelation,
    },
    ResultRow, SQLError,
};

pub(super) fn append_virtual_columns(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    request: &CatalogRequest,
    out: &mut Vec<ResultRow>,
) -> Result<(), SQLError> {
    for relation in VirtualRelation::ALL {
        if !request.matches_relation(relation.namespace(), relation.name()) {
            continue;
        }
        let system = SystemRelation::Projected(*relation);
        let security =
            catalog.relation_security_names(&catalog.system_relation_security(system))?;
        let definitions = &catalog.snapshot().definitions;
        for (index, (name, ty)) in relation.schema().into_iter().enumerate() {
            if !request.matches_name("column_name", &name)
                || !TableAclPrivilege::COLUMN_ALL.into_iter().any(|privilege| {
                    system_relations::has_column_privilege(
                        system,
                        &security,
                        &name,
                        resolution.current_user(),
                        TablePrivilegeCheck {
                            privilege,
                            grant_option: false,
                        },
                        &definitions.roles,
                        &definitions.role_memberships,
                    )
                })
            {
                continue;
            }
            out.push(information_schema_column_row(
                None,
                (catalog, resolution, request),
                (relation.namespace().into(), relation.name().into()),
                index,
                &ColumnDef::nullable(name, ty),
                relation.kind() == "table",
                None,
            )?);
        }
    }
    Ok(())
}

pub(super) fn append_view_columns(
    context: &CatalogContext<'_>,
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    request: &CatalogRequest,
    out: &mut Vec<ResultRow>,
) -> Result<(), SQLError> {
    for (identity, stored) in catalog.snapshot().definitions.views.iter() {
        if stored.kind != crate::catalog::view::StoredViewKind::View
            || !request.matches_relation(&identity.schema, &identity.name)
            || !catalog.view_is_visible_to(stored, resolution.current_user())
        {
            continue;
        }
        let view_name = identity.qualified_name();
        let updatability = request
            .includes("is_updatable")
            .then(|| {
                context
                    .views
                    .view_updatability_with_catalog(&view_name, catalog, resolution)
            })
            .transpose()?;
        let columns = view_columns_for(context, catalog, resolution, stored)?;
        for (idx, column) in columns.iter().enumerate() {
            if !request.matches_name("column_name", &column.name)
                || !catalog.view_column_is_visible_to(
                    stored,
                    &column.name,
                    resolution.current_user(),
                )
            {
                continue;
            }
            out.push(information_schema_column_row(
                Some(&crate::catalog::projection::CatalogOutput(*context)),
                (catalog, resolution, request),
                (identity.schema.clone(), identity.name.clone()),
                idx,
                column,
                updatability
                    .as_ref()
                    .and_then(|value| value.catalog_columns.get(idx))
                    .copied()
                    .unwrap_or(false),
                None,
            )?);
        }
    }
    Ok(())
}
