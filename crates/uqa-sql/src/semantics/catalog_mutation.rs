//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Mutation rules for SQL virtual catalog relations.

use crate::catalog::resolution::RelationNameResolution;
use crate::plan::{CommandPlan, MergeWhenPlan};
use crate::SQLError;

pub fn virtual_relation_mutation_error(
    resolution: &RelationNameResolution,
    command: &CommandPlan,
) -> Option<SQLError> {
    let target = command.mutation_target()?;
    if crate::catalog::resolve_virtual_relation(resolution.search_path(), target)
        != Some(crate::catalog::VirtualRelation::PgPreparedStatements)
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
