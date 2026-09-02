//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::collections::BTreeSet;

use uqa_sql::SQLError;

use super::{BaseColumn, SourceLineage};
use crate::sql::select::CteScope;

pub(super) fn ensure_required_select(
    lineage: &SourceLineage,
    required: &BTreeSet<BaseColumn>,
    ctes: &CteScope,
) -> Result<(), SQLError> {
    let catalog = ctes.catalog_read_view()?;
    let mut resolution = ctes.relation_name_resolution()?;
    resolution.set_lookup_mode(crate::engine_capabilities::RelationLookupMode::Bound);
    let subject = ctes.privilege_subject()?;
    for (table, columns) in &lineage.tables {
        let table_required = required
            .iter()
            .filter(|column| column.table == *table)
            .map(|column| column.column.as_str())
            .collect::<BTreeSet<_>>();
        if let Some(snapshot) = catalog.table_resolved(&resolution, table)? {
            if table_required.is_empty() {
                let permitted = catalog.table_has_privilege_to(
                    snapshot,
                    subject,
                    crate::engine_table_security::TableAclPrivilege::Select,
                ) || columns.iter().any(|column| {
                    catalog.table_column_has_privilege_to(
                        snapshot,
                        column,
                        subject,
                        crate::engine_table_security::TableAclPrivilege::Select,
                    )
                });
                if !permitted {
                    return relation_permission_denied(table, "table");
                }
            } else {
                for column in table_required {
                    let permitted = if columns.contains(column) {
                        catalog.table_column_has_privilege_to(
                            snapshot,
                            column,
                            subject,
                            crate::engine_table_security::TableAclPrivilege::Select,
                        )
                    } else {
                        catalog.table_has_privilege_to(
                            snapshot,
                            subject,
                            crate::engine_table_security::TableAclPrivilege::Select,
                        )
                    };
                    if !permitted {
                        return relation_permission_denied(table, "table");
                    }
                }
            }
            continue;
        }
        if let Some(view) = catalog.view_resolved(&resolution, table)? {
            let kind = match view.kind {
                crate::StoredViewKind::View => "view",
                crate::StoredViewKind::Materialized => "materialized view",
            };
            if table_required.is_empty() {
                let permitted = catalog.view_has_privilege_to(
                    view,
                    subject,
                    crate::engine_table_security::TableAclPrivilege::Select,
                ) || columns.iter().any(|column| {
                    catalog.view_column_has_privilege_to(
                        view,
                        column,
                        subject,
                        crate::engine_table_security::TableAclPrivilege::Select,
                    )
                });
                if !permitted {
                    return relation_permission_denied(table, kind);
                }
            } else {
                for column in table_required {
                    let permitted = if columns.contains(column) {
                        catalog.view_column_has_privilege_to(
                            view,
                            column,
                            subject,
                            crate::engine_table_security::TableAclPrivilege::Select,
                        )
                    } else {
                        catalog.view_has_privilege_to(
                            view,
                            subject,
                            crate::engine_table_security::TableAclPrivilege::Select,
                        )
                    };
                    if !permitted {
                        return relation_permission_denied(table, kind);
                    }
                }
            }
            continue;
        }
        return Err(SQLError::UnknownTable(table.clone()));
    }
    Ok(())
}

fn relation_permission_denied<T>(table: &str, kind: &str) -> Result<T, SQLError> {
    let relation = crate::RelationIdentity::from_legacy_name(table).map_err(SQLError::Internal)?;
    Err(SQLError::Routine {
        sqlstate: "42501".into(),
        message: format!("permission denied for {kind} {}", relation.name),
    })
}
