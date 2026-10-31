//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relation-target locking for table functions.

use super::RowLockContext;
use uqa_core::memory::ProductionControl;
use uqa_sql::ast::OperatorJoinRelations;
use uqa_sql::{
    binding::production::BindingNameSet, catalog::resolution::RelationResolution, SQLError,
};

pub(super) fn lock_table_function_relations<S: Clone + Send + Sync + 'static>(
    context: RowLockContext<'_, S>,
    relations: Option<&OperatorJoinRelations>,
    relations_bound: bool,
    locked: &mut BindingNameSet,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    let Some(relations) = relations else {
        return Ok(());
    };
    for relation in [&relations.left, &relations.right] {
        let resolved =
            context
                .catalog
                .resolve_relation_with_control(relation, relations_bound, control)?;
        let RelationResolution::Found(table, "table") = &*resolved else {
            continue;
        };
        if locked.insert_with_control(table, control)? {
            context
                .session
                .lock_relation(table, crate::row_locks::RelationLockMode::AccessShare)?;
        }
    }
    Ok(())
}
