//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{context::PrivilegeScope, ownership::Required, SourceLineage};
use crate::{catalog::resolution::RelationLookupMode, SQLError};

pub(super) fn ensure_required_select(
    lineage: &SourceLineage,
    required: &Required,
    scope: &PrivilegeScope<'_, impl AsRef<[crate::plan::QueryPlan]>>,
) -> Result<(), SQLError> {
    let control = scope.control();
    let mut resolution = scope.resolution.copy_with_control(&control)?;
    resolution.set_lookup_mode(RelationLookupMode::Bound);
    let subject = scope.privilege_subject()?;
    for (table, columns) in &lineage.tables {
        let relation = scope
            .catalog
            .relation_with_control(&resolution, table, &control)?
            .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
        let mut table_required = required
            .iter()
            .filter(|column| column.table == *table)
            .map(|column| column.column.as_str())
            .peekable();
        if table_required.peek().is_none() {
            let mut permitted = scope.catalog.has_select_privilege_with_control(
                &resolution,
                &relation,
                None,
                subject,
                &control,
            )?;
            // Foreign relation ACL lookup can report metadata errors even after a table grant.
            let inspect_columns = !permitted
                || matches!(
                    relation.kind,
                    super::context::PrivilegeRelationKind::ForeignTable
                );
            if inspect_columns {
                for column in columns {
                    if scope.catalog.has_select_privilege_with_control(
                        &resolution,
                        &relation,
                        Some(column),
                        subject,
                        &control,
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
                if !scope.catalog.has_select_privilege_with_control(
                    &resolution,
                    &relation,
                    column,
                    subject,
                    &control,
                )? {
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
