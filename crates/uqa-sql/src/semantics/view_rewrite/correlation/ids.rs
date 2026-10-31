//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected DML query slots retain an admitted ordered node for each distinct identifier.

use super::{
    ConflictActionPlan, ConflictPlan, DeletePlan, InsertPlan, MergePlan, MergeWhenPlan,
    ProjectionPlan, SQLError, ScalarExpr, UpdatePlan,
};
use uqa_core::memory::{OwnedSet, ProductionControl};
pub(super) type SubqueryIds = OwnedSet<usize>;

fn collect<'a>(
    ids: &mut SubqueryIds,
    expressions: impl IntoIterator<Item = &'a ScalarExpr>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    for expression in expressions {
        expression.try_visit(&mut |node| {
            control.check()?;
            if let ScalarExpr::ScalarSubquery(id)
            | ScalarExpr::Exists { subquery: id, .. }
            | ScalarExpr::InSubquery { subquery: id, .. } = node
            {
                if let Some(budget) = control.budget() {
                    ids.insert_budgeted(*id, budget)?;
                } else {
                    ids.insert(*id);
                }
            }
            Ok::<_, SQLError>(true)
        })?;
    }
    Ok(())
}

pub(in crate::semantics::view_rewrite) fn returning(
    input: &[ProjectionPlan],
    control: &ProductionControl<'_>,
) -> Result<SubqueryIds, SQLError> {
    let mut ids = SubqueryIds::new();
    collect(
        &mut ids,
        input.iter().map(|projection| &projection.expr),
        control,
    )?;
    Ok(ids)
}

pub(in crate::semantics::view_rewrite) fn update(
    input: &UpdatePlan,
    control: &ProductionControl<'_>,
) -> Result<SubqueryIds, SQLError> {
    let mut ids = SubqueryIds::new();
    collect(
        &mut ids,
        input
            .assignments
            .iter()
            .flat_map(crate::plan::AssignmentPlan::expressions),
        control,
    )?;
    collect(&mut ids, input.predicate.iter(), control)?;
    Ok(ids)
}

pub(in crate::semantics::view_rewrite) fn delete(
    input: &DeletePlan,
    control: &ProductionControl<'_>,
) -> Result<SubqueryIds, SQLError> {
    let mut ids = SubqueryIds::new();
    collect(&mut ids, input.predicate.iter(), control)?;
    Ok(ids)
}

pub(in crate::semantics::view_rewrite) fn conflict(
    input: &InsertPlan,
    control: &ProductionControl<'_>,
) -> Result<SubqueryIds, SQLError> {
    let mut ids = SubqueryIds::new();
    if let Some(ConflictPlan {
        action:
            ConflictActionPlan::Update {
                assignments,
                predicate,
            },
        ..
    }) = &input.on_conflict
    {
        collect(
            &mut ids,
            assignments
                .iter()
                .flat_map(crate::plan::AssignmentPlan::expressions),
            control,
        )?;
        collect(&mut ids, predicate.iter().map(Box::as_ref), control)?;
    }
    Ok(ids)
}

pub(in crate::semantics::view_rewrite) fn merge_matched(
    input: &MergePlan,
    control: &ProductionControl<'_>,
) -> Result<SubqueryIds, SQLError> {
    let mut ids = SubqueryIds::new();
    collect(&mut ids, std::iter::once(&input.join_condition), control)?;
    for clause in &input.when_clauses {
        match clause {
            MergeWhenPlan::UpdateMatched {
                condition,
                assignments,
            } => {
                collect(&mut ids, condition.iter(), control)?;
                collect(
                    &mut ids,
                    assignments
                        .iter()
                        .flat_map(crate::plan::AssignmentPlan::expressions),
                    control,
                )?;
            }
            MergeWhenPlan::DeleteMatched { condition }
            | MergeWhenPlan::NothingMatched { condition } => {
                collect(&mut ids, condition.iter(), control)?;
            }
            _ => {}
        }
    }
    Ok(ids)
}

pub(in crate::semantics::view_rewrite) fn merge_target_only(
    input: &MergePlan,
    control: &ProductionControl<'_>,
) -> Result<SubqueryIds, SQLError> {
    let mut ids = SubqueryIds::new();
    for clause in &input.when_clauses {
        match clause {
            MergeWhenPlan::UpdateNotMatchedBySource {
                condition,
                assignments,
            } => {
                collect(&mut ids, condition.iter(), control)?;
                collect(
                    &mut ids,
                    assignments
                        .iter()
                        .flat_map(crate::plan::AssignmentPlan::expressions),
                    control,
                )?;
            }
            MergeWhenPlan::DeleteNotMatchedBySource { condition }
            | MergeWhenPlan::NothingNotMatchedBySource { condition } => {
                collect(&mut ids, condition.iter(), control)?;
            }
            _ => {}
        }
    }
    Ok(ids)
}
