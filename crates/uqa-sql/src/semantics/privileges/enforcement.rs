//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{context::PrivilegeScope, BaseColumn, SourceLineage};
use crate::{catalog::resolution::RelationLookupMode, SQLError};
use std::collections::BTreeSet;

pub(super) fn ensure_required_select(
    lineage: &SourceLineage,
    required: &BTreeSet<BaseColumn>,
    scope: &PrivilegeScope<'_>,
) -> Result<(), SQLError> {
    let mut resolution = scope.resolution.clone();
    resolution.set_lookup_mode(RelationLookupMode::Bound);
    let subject = scope.privilege_subject()?;
    for (table, columns) in &lineage.tables {
        let relation = scope
            .catalog
            .relation(&resolution, table)?
            .ok_or_else(|| SQLError::UnknownTable(table.clone()))?;
        let table_required = required
            .iter()
            .filter(|column| &column.table == table)
            .map(|column| column.column.as_str())
            .collect::<BTreeSet<_>>();
        if table_required.is_empty() {
            let mut permitted =
                scope
                    .catalog
                    .has_select_privilege(&resolution, &relation, None, subject)?;
            // Foreign relation ACL lookup can report metadata errors even after a table grant.
            let inspect_columns = !permitted
                || matches!(
                    relation.kind,
                    super::context::PrivilegeRelationKind::ForeignTable
                );
            if inspect_columns {
                for column in columns {
                    if scope.catalog.has_select_privilege(
                        &resolution,
                        &relation,
                        Some(column),
                        subject,
                    )? {
                        permitted = true;
                        break;
                    }
                }
            }
            if !permitted {
                return relation_permission_denied(table, relation.kind.description());
            }
        } else {
            for column in table_required {
                let column = columns.contains(column).then_some(column);
                if !scope
                    .catalog
                    .has_select_privilege(&resolution, &relation, column, subject)?
                {
                    return relation_permission_denied(table, relation.kind.description());
                }
            }
        }
    }
    Ok(())
}
fn relation_permission_denied<T>(table: &str, kind: &str) -> Result<T, SQLError> {
    let relation =
        uqa_core::RelationIdentity::from_legacy_name(table).map_err(SQLError::Internal)?;
    Err(SQLError::Routine {
        sqlstate: "42501".into(),
        message: format!("permission denied for {kind} {}", relation.name),
    })
}
