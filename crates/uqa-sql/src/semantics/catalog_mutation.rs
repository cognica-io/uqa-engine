//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Mutation rules for SQL virtual catalog relations.

use crate::catalog::{
    analysis::AnalysisCatalog,
    resolution::{RelationLookupMode, RelationNameResolution},
    VirtualRelation,
};
use crate::plan::{CommandPlan, MergeWhenPlan};
use crate::SQLError;

/// Check spelling before requesting a catalog snapshot; an earlier user relation can still take precedence.
pub fn virtual_relation_mutation_candidate(command: &CommandPlan) -> bool {
    command.mutation_target().is_some_and(|target| {
        crate::catalog::resolve_virtual_relation(&[], target)
            == Some(VirtualRelation::PgPreparedStatements)
    })
}

pub fn virtual_relation_mutation_error(
    catalog: &dyn AnalysisCatalog,
    resolution: &RelationNameResolution,
    command: &CommandPlan,
) -> Result<Option<SQLError>, SQLError> {
    if !virtual_relation_mutation_candidate(command) {
        return Ok(None);
    }
    let Some(target) = command.mutation_target() else {
        return Ok(None);
    };
    let bound = match command {
        CommandPlan::Insert(plan) => plan.target_relation_bound,
        CommandPlan::Update(plan) => plan.target_relation_bound,
        CommandPlan::Delete(plan) => plan.target_relation_bound,
        _ => false,
    };
    let mut resolution = resolution.clone();
    if bound {
        resolution.set_lookup_mode(RelationLookupMode::Bound);
    }
    if catalog
        .virtual_relation_schema(&resolution, target)?
        .is_none()
    {
        return Ok(None);
    }
    Ok(prepared_mutation_error(command))
}

fn prepared_mutation_error(command: &CommandPlan) -> Option<SQLError> {
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
