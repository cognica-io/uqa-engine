//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relation-target locking for table functions.

use crate::Engine;
use uqa_sql::ast::OperatorJoinRelations;
use uqa_sql::SQLError;

pub(super) fn lock_table_function_relations(
    engine: &Engine,
    relations: Option<&OperatorJoinRelations>,
    relations_bound: bool,
    locked: &mut std::collections::BTreeSet<String>,
) -> Result<(), SQLError> {
    let Some(relations) = relations else {
        return Ok(());
    };
    for relation in [&relations.left, &relations.right] {
        let Some((table, "table")) =
            engine.try_resolve_relation_kind_for_query(relation, relations_bound)?
        else {
            continue;
        };
        if locked.insert(table.clone()) {
            engine.lock_relation(&table, crate::row_locks::RelationLockMode::AccessShare)?;
        }
    }
    Ok(())
}
