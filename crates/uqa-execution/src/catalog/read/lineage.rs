//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::CatalogReadView;
use crate::catalog::{security::table::TableAclPrivilege, view::StoredViewKind};
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
        Ok(None)
    }
    fn has_select_privilege(
        &self,
        resolution: &RelationNameResolution,
        relation: &PrivilegeRelation,
        column: Option<&str>,
        subject: &str,
    ) -> Result<bool, SQLError> {
        let privilege = TableAclPrivilege::Select;
        match relation.kind {
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
