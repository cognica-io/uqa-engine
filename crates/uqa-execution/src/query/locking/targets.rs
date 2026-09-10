//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relation-target locking for table functions.

use super::RowLockContext;
use uqa_sql::ast::OperatorJoinRelations;
use uqa_sql::SQLError;

pub(super) fn lock_table_function_relations<S: Clone + Send + Sync + 'static>(
    context: RowLockContext<'_, S>,
    relations: Option<&OperatorJoinRelations>,
    relations_bound: bool,
    locked: &mut std::collections::BTreeSet<String>,
) -> Result<(), SQLError> {
    let Some(relations) = relations else {
        return Ok(());
    };
    for relation in [&relations.left, &relations.right] {
        let Some((table, "table")) = context
            .catalog
            .resolve_relation(relation, relations_bound)?
        else {
            continue;
        };
        if locked.insert(table.clone()) {
            context
                .session
                .lock_relation(&table, crate::row_locks::RelationLockMode::AccessShare)?;
        }
    }
    Ok(())
}
