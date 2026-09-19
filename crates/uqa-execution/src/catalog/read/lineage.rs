//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::CatalogReadView;
use crate::catalog::{security::table::TableAclPrivilege, view::StoredViewKind};
use uqa_sql::catalog::roles::RoleReference;
use uqa_sql::{
    catalog::resolution::RelationNameResolution,
    semantics::privileges::context::{PrivilegeCatalog, PrivilegeRelation, PrivilegeRelationKind},
    SQLError,
};
impl PrivilegeCatalog for CatalogReadView {
    fn relation(
        &self,
        resolution: &RelationNameResolution,
        name: &str,
    ) -> Result<Option<PrivilegeRelation>, SQLError> {
        if let Some(canonical) = self.table_name_resolved(resolution, name)? {
            let table = self
                .table_resolved(resolution, &canonical)?
                .ok_or_else(|| SQLError::UnknownTable(canonical.clone()))?;
            return Ok(Some(PrivilegeRelation {
                columns: table
                    .columns
                    .iter()
                    .map(|column| column.name.clone())
                    .collect(),
                canonical,
                kind: PrivilegeRelationKind::Table,
            }));
        }
        if let Some(canonical) = self.view_name_resolved(resolution, name)? {
            let view = self
                .view_resolved(resolution, &canonical)?
                .ok_or_else(|| SQLError::UnknownTable(canonical.clone()))?;
            let columns = view.output_columns.clone().ok_or_else(|| {
                SQLError::Internal(format!(
                    "loaded view `{canonical}` has no durable public column metadata"
                ))
            })?;
            let kind = match view.kind {
                StoredViewKind::View => PrivilegeRelationKind::View,
                StoredViewKind::Materialized => PrivilegeRelationKind::MaterializedView,
            };
            return Ok(Some(PrivilegeRelation {
                columns,
                canonical,
                kind,
            }));
        }
        if let Some((canonical, table)) = self.foreign_table_entry_resolved(resolution, name)? {
            return Ok(Some(PrivilegeRelation {
                columns: table
                    .columns
                    .iter()
                    .map(|column| column.name.clone())
                    .collect(),
                canonical,
                kind: PrivilegeRelationKind::ForeignTable,
            }));
        }
        if let Some((canonical, _)) = self
            .relation_kind_resolution(resolution, name)?
            .into_found()
        {
            if let Some(relation) =
                uqa_sql::catalog::SystemRelation::from_qualified_name(&canonical)
            {
                return Ok(Some(PrivilegeRelation {
                    canonical,
                    columns: relation.column_names(),
                    kind: PrivilegeRelationKind::System(relation),
                }));
            }
        }
        Ok(None)
    }
    fn has_select_privilege(
        &self,
        resolution: &RelationNameResolution,
        relation: &PrivilegeRelation,
        column: Option<&str>,
        subject: &RoleReference,
    ) -> Result<bool, SQLError> {
        let privilege = TableAclPrivilege::Select;
        match relation.kind {
            PrivilegeRelationKind::System(system) => {
                use uqa_sql::catalog::security::{
                    system_relations::{self, SystemRelationSecurityCatalog},
                    table::TablePrivilegeCheck,
                };
                let security = self
                    .system_relation_security(system)
                    .resolve(&self.snapshot.definitions.roles)
                    .map_err(SQLError::Internal)?;
                let check = TablePrivilegeCheck {
                    privilege,
                    grant_option: false,
                };
                let definitions = &self.snapshot.definitions;
                Ok(column.map_or_else(
                    || {
                        system_relations::has_table_privilege(
                            system,
                            &security,
                            subject,
                            check,
                            &definitions.roles,
                            &definitions.role_memberships,
                        )
                    },
                    |column| {
                        system_relations::has_column_privilege(
                            system,
                            &security,
                            column,
                            subject,
                            check,
                            &definitions.roles,
                            &definitions.role_memberships,
                        )
                    },
                ))
            }
            PrivilegeRelationKind::Table => {
                let table = self
                    .table_resolved(resolution, &relation.canonical)?
                    .ok_or_else(|| SQLError::UnknownTable(relation.canonical.clone()))?;
                Ok(column.map_or_else(
                    || self.table_has_privilege_to(table, subject, privilege),
                    |column| self.table_column_has_privilege_to(table, column, subject, privilege),
                ))
            }
            PrivilegeRelationKind::View | PrivilegeRelationKind::MaterializedView => {
                let view = self
                    .view_resolved(resolution, &relation.canonical)?
                    .ok_or_else(|| SQLError::UnknownTable(relation.canonical.clone()))?;
                Ok(column.map_or_else(
                    || self.view_has_privilege_to(view, subject, privilege),
                    |column| self.view_column_has_privilege_to(view, column, subject, privilege),
                ))
            }
            PrivilegeRelationKind::ForeignTable => column.map_or_else(
                || self.foreign_table_has_privilege_to(&relation.canonical, subject, privilege),
                |column| {
                    self.foreign_table_column_has_privilege_to(
                        &relation.canonical,
                        column,
                        subject,
                        privilege,
                    )
                },
            ),
        }
    }
}
