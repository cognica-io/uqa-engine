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
    command
        .mutation_target()
        .is_some_and(|target| session_metadata_relation(target).is_some())
}

fn session_metadata_relation(target: &str) -> Option<VirtualRelation> {
    crate::catalog::resolve_virtual_relation(&[], target).filter(|relation| {
        matches!(
            relation,
            VirtualRelation::PgPreparedStatements | VirtualRelation::PgCursors
        )
    })
}

pub fn virtual_relation_mutation_error(
    catalog: &dyn AnalysisCatalog,
    resolution: &RelationNameResolution,
    command: &CommandPlan,
) -> Result<Option<SQLError>, SQLError> {
    let Some(target) = command.mutation_target() else {
        return Ok(None);
    };
    let Some(relation) = session_metadata_relation(target) else {
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
    Ok(mutation_error(command, relation))
}

fn mutation_error(command: &CommandPlan, relation: VirtualRelation) -> Option<SQLError> {
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
        message: format!("cannot {action} view \"{}\"", relation.name()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_catalog_mutations_retain_each_views_name_and_operation() {
        for relation in [
            VirtualRelation::PgCursors,
            VirtualRelation::PgPreparedStatements,
        ] {
            for (sql, action) in [
                (
                    format!("INSERT INTO {} (name) VALUES ('x')", relation.name()),
                    "insert into",
                ),
                (
                    format!("UPDATE {} SET name = 'x'", relation.name()),
                    "update",
                ),
                (format!("DELETE FROM {}", relation.name()), "delete from"),
            ] {
                let crate::plan::UnifiedPlan::Command(command) =
                    crate::plan::UnifiedPlan::lower(crate::compile(&sql).unwrap().remove(0))
                else {
                    panic!("mutation fixture produced a query");
                };
                assert!(virtual_relation_mutation_candidate(&command));
                let error = mutation_error(&command, relation).unwrap();
                assert_eq!(error.sqlstate(), Some("55000"));
                assert_eq!(
                    error.to_string(),
                    format!("cannot {action} view \"{}\"", relation.name())
                );
            }
        }
        assert_eq!(session_metadata_relation("public.pg_cursors"), None);
        assert_eq!(session_metadata_relation("pg_catalog.\"PG_CURSORS\""), None);
    }
}
