//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Delete foreign columns through the same dependency lifecycle as dependent type and routine removal.

use super::ForeignTableAlterContext;
use crate::schema::deletion::{perform_deletion, required_address};
use uqa_core::RelationIdentity;
use uqa_sql::{ast::DropColumnAction, SQLError};

pub(super) fn drop_columns(
    context: &ForeignTableAlterContext<'_>,
    relation: &RelationIdentity,
    actions: &[DropColumnAction],
) -> Result<(), SQLError> {
    let deletion = context.deletion.catalog_removal_context();
    let name = relation.qualified_name();
    deletion
        .events
        .ensure_no_pending_trigger_events(&name, "ALTER TABLE")?;
    for action in actions {
        let table = context
            .catalog
            .table(relation)
            .ok_or_else(|| SQLError::Internal("altered foreign table disappeared".into()))?;
        if !uqa_sql::schema::columns::validate_drop_column(
            &table.columns,
            &name,
            &action.name,
            action.if_exists,
        )? {
            context
                .notices
                .push(uqa_sql::schema::columns::missing_drop_column_notice(
                    &relation.name,
                    &action.name,
                ));
            continue;
        }
        perform_deletion(
            &deletion,
            |dependencies| {
                Ok(vec![required_address(
                    dependencies.relation_address(relation, Some(&action.name)),
                    || format!("column {} of foreign table {name}", action.name),
                )?])
            },
            action.cascade,
        )?;
    }
    Ok(())
}
