//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected relation descriptors retain only their admitted names; authorization borrows the captured ACL generation.

use super::{
    CatalogReadView, PrivilegeRelation, PrivilegeRelationKind, RelationNameResolution,
    RoleReference, SQLError, StoredViewKind,
};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

fn columns<'a>(
    names: impl IntoIterator<Item = &'a str>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<String>>, SQLError> {
    let mut output = ProductionVec::new(*control);
    for name in names {
        output.push_produced(control.copy_text(name)?)?;
    }
    Ok(output.finish()?)
}

pub(super) fn relation(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    name: &str,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<PrivilegeRelation>>, SQLError> {
    if let Some(canonical) = catalog.table_name_resolved_with_control(resolution, name, control)? {
        let table = catalog
            .table_resolved_with_control(resolution, &canonical, control)?
            .ok_or_else(|| SQLError::UnknownTable(canonical.to_string()))?;
        return PrivilegeRelation::from_columns(
            canonical,
            columns(
                table.columns.iter().map(|column| column.name.as_str()),
                control,
            )?,
            PrivilegeRelationKind::Table,
            control,
        )
        .map(Some);
    }
    if let Some(canonical) = catalog.view_name_resolved_with_control(resolution, name, control)? {
        let view = catalog
            .view_resolved_with_control(resolution, &canonical, control)?
            .ok_or_else(|| SQLError::UnknownTable(canonical.to_string()))?;
        let names = view.output_columns.as_ref().ok_or_else(|| {
            SQLError::Internal(format!(
                "loaded view `{}` has no durable public column metadata",
                canonical.as_str()
            ))
        })?;
        let kind = match view.kind {
            StoredViewKind::View => PrivilegeRelationKind::View,
            StoredViewKind::Materialized => PrivilegeRelationKind::MaterializedView,
        };
        return PrivilegeRelation::from_columns(
            canonical,
            columns(names.iter().map(String::as_str), control)?,
            kind,
            control,
        )
        .map(Some);
    }
    let candidates = catalog
        .relation_names()
        .relation_lookup_candidates_with_control(resolution, name, control)?;
    for candidate in candidates.iter() {
        control.check()?;
        if let Some(table) = catalog.snapshot.definitions.foreign_tables.get(candidate) {
            return PrivilegeRelation::from_columns(
                candidate.qualified_name_with_control(control)?,
                columns(
                    table.columns.iter().map(|column| column.name.as_str()),
                    control,
                )?,
                PrivilegeRelationKind::ForeignTable,
                control,
            )
            .map(Some);
        }
        if catalog.relation_exists(candidate) {
            break;
        }
    }
    drop(candidates);
    let selected = catalog.relation_kind_resolution_with_control(resolution, name, control)?;
    if let uqa_sql::catalog::resolution::RelationResolution::Found(canonical, _) = &*selected {
        if let Some(system) = uqa_sql::catalog::SystemRelation::from_qualified_name(canonical) {
            return PrivilegeRelation::from_columns(
                control.copy_text(canonical)?,
                system.column_names_with_control(control)?,
                PrivilegeRelationKind::System(system),
                control,
            )
            .map(Some);
        }
    }
    Ok(None)
}

pub(super) fn select(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    relation: &PrivilegeRelation,
    column: Option<&str>,
    subject: &RoleReference,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    let definitions = &catalog.snapshot.definitions;
    let privileges = uqa_sql::catalog::security::table::selection::SelectPrivileges {
        subject,
        roles: &definitions.roles,
        memberships: &definitions.role_memberships,
        control: *control,
    };
    match relation.kind {
        PrivilegeRelationKind::Table => {
            let table = catalog
                .table_resolved_with_control(resolution, &relation.canonical, control)?
                .ok_or_else(|| SQLError::UnknownTable(relation.canonical.clone()))?;
            invalid_relation_acl_denies(privileges.relation(&table.security, column))
        }
        PrivilegeRelationKind::View | PrivilegeRelationKind::MaterializedView => {
            let view = catalog
                .view_resolved_with_control(resolution, &relation.canonical, control)?
                .ok_or_else(|| SQLError::UnknownTable(relation.canonical.clone()))?;
            invalid_relation_acl_denies(privileges.relation(&view.security, column))
        }
        PrivilegeRelationKind::ForeignTable => {
            let reference = uqa_core::RelationIdentity::try_borrowed_reference(&relation.canonical)
                .map_err(|error| {
                    SQLError::Internal(format!(
                        "resolve catalog foreign table `{}`: {error}",
                        relation.canonical
                    ))
                })?;
            let identity = reference
                .identity_with_control(uqa_core::RelationName::decoded("public"), control)?;
            let security = definitions
                .foreign_table_security
                .get(&*identity)
                .ok_or_else(|| {
                    SQLError::Internal(format!(
                        "catalog foreign table `{}` has no security metadata",
                        relation.canonical
                    ))
                })?;
            privileges.relation(security, column)
        }
        PrivilegeRelationKind::System(system) => {
            let security = definitions
                .system_relation_security
                .iter()
                .find(|(name, _)| name.schema == system.namespace() && name.name == system.name())
                .map(|(_, security)| security);
            privileges.system(system, security, column)
        }
    }
}

fn invalid_relation_acl_denies(result: Result<bool, SQLError>) -> Result<bool, SQLError> {
    match result {
        Err(SQLError::Internal(_)) => Ok(false),
        result => result,
    }
}

#[cfg(test)]
mod tests;
