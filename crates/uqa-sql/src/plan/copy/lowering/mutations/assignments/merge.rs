//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::super::input::{scalars::construct_scalars, ScalarInput, TextInput};
use super::{input, targets, Copying, Result};
use crate::{
    ast::{Expr, MergeWhen},
    plan::{retirement::Pending, MergeWhenPlan},
};
use uqa_core::memory::BudgetedVec;

pub(in crate::plan::copy::lowering::mutations) async fn construct_merge_whens<
    's,
    I: ScalarInput<&'s Expr> + TextInput<&'s str>,
>(
    source: &'s [MergeWhen],
    copying: &mut Copying<'_>,
    input: &mut I,
) -> Result<Pending<Vec<MergeWhenPlan>>> {
    input.checkpoint().await?;
    copying.check()?;
    let mut values = Pending::new(BudgetedVec::new(
        copying.control.budget().expect("MERGE allowance"),
    ));
    values.reserve(source.len())?;
    copying
        .fields
        .as_mut()
        .expect("MERGE records")
        .reserve(source.len())?;
    for clause in source {
        input.checkpoint().await?;
        let mut field = copying.begin_input_field()?;
        let clause = construct_merge_when(clause, &mut field, input).await?;
        values.push(clause.take()).expect("admitted MERGE slot");
        copying.finish_input_field("item", field);
    }
    let (values, memory) = values.take().into_parts();
    copying.absorb(Some(memory));
    Ok(Pending::new(values))
}

async fn construct_merge_when<'s, I: ScalarInput<&'s Expr> + TextInput<&'s str>>(
    source: &'s MergeWhen,
    copying: &mut Copying<'_>,
    input: &mut I,
) -> Result<Pending<MergeWhenPlan>> {
    let condition = match source {
        MergeWhen::UpdateMatched { condition, .. }
        | MergeWhen::UpdateNotMatchedBySource { condition, .. }
        | MergeWhen::DeleteMatched { condition }
        | MergeWhen::DeleteNotMatchedBySource { condition }
        | MergeWhen::InsertNotMatched { condition, .. }
        | MergeWhen::NothingMatched { condition }
        | MergeWhen::NothingNotMatched { condition }
        | MergeWhen::NothingNotMatchedBySource { condition } => condition,
    };
    let condition = input::optional("condition", condition.as_ref(), copying, input).await?;
    Ok(Pending::new(match source {
        MergeWhen::UpdateMatched { assignments, .. }
        | MergeWhen::UpdateNotMatchedBySource { assignments, .. } => {
            let mut field = copying.begin_input_field()?;
            let assignments = input::construct_assignments(assignments, &mut field, input).await?;
            copying.finish_input_field("assignments", field);
            match source {
                MergeWhen::UpdateMatched { .. } => MergeWhenPlan::UpdateMatched {
                    condition: condition.take(),
                    assignments: assignments.take(),
                },
                _ => MergeWhenPlan::UpdateNotMatchedBySource {
                    condition: condition.take(),
                    assignments: assignments.take(),
                },
            }
        }
        MergeWhen::DeleteMatched { .. } => MergeWhenPlan::DeleteMatched {
            condition: condition.take(),
        },
        MergeWhen::DeleteNotMatchedBySource { .. } => MergeWhenPlan::DeleteNotMatchedBySource {
            condition: condition.take(),
        },
        MergeWhen::InsertNotMatched {
            columns,
            overriding,
            values,
            ..
        } => {
            // Values precede target indirection in the established query-arena trace.
            let mut field = copying.begin_input_field()?;
            let values =
                construct_scalars(values, &mut field, input, |value| value, |_, value| value)
                    .await?;
            copying.finish_input_field("values", field);
            let mut field = copying.begin_input_field()?;
            let columns = targets::construct_targets(columns, &mut field, input).await?;
            copying.finish_input_field("columns", field);
            MergeWhenPlan::InsertNotMatched {
                condition: condition.take(),
                columns: columns.take(),
                overriding: *overriding,
                values: values.take(),
            }
        }
        MergeWhen::NothingMatched { .. } => MergeWhenPlan::NothingMatched {
            condition: condition.take(),
        },
        MergeWhen::NothingNotMatched { .. } => MergeWhenPlan::NothingNotMatched {
            condition: condition.take(),
        },
        MergeWhen::NothingNotMatchedBySource { .. } => MergeWhenPlan::NothingNotMatchedBySource {
            condition: condition.take(),
        },
    }))
}
