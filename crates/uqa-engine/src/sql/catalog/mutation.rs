//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Mutation capabilities of virtual session catalog views.

use crate::engine_capabilities::RelationNameResolution;
use uqa_planner::{CommandPlan, MergeWhenPlan};
use uqa_sql::SQLError;

pub(in crate::sql) fn virtual_relation_mutation_error(
    resolution: &RelationNameResolution,
    command: &CommandPlan,
) -> Option<SQLError> {
    let target = command.mutation_target()?;
    if super::resolve_virtual_relation(resolution, target)
        != Some(super::VirtualRelation::PgPreparedStatements)
    {
        return None;
    }
    let action = match command {
        CommandPlan::Insert(_) => "insert into",
        CommandPlan::Update(_) => "update",
        CommandPlan::Delete(_) => "delete from",
        CommandPlan::Merge(merge) => merge.when_clauses.iter().find_map(|clause| match clause {
            MergeWhenPlan::InsertNotMatched { .. } => Some("insert into"),
            MergeWhenPlan::UpdateMatched { .. }
            | MergeWhenPlan::UpdateNotMatchedBySource { .. } => Some("update"),
            MergeWhenPlan::DeleteMatched { .. }
            | MergeWhenPlan::DeleteNotMatchedBySource { .. } => Some("delete from"),
            _ => None,
        })?,
        _ => return None,
    };
    Some(SQLError::Routine {
        sqlstate: "55000".into(),
        message: format!("cannot {action} view \"pg_prepared_statements\""),
    })
}
