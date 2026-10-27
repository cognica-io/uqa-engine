//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Copy only the descriptive fields of a completed, scope-aware cost traversal.

use super::{ExplainNodeEstimate, ExplainPlanEstimate};
use crate::plan_cost::nodes::{PlanCostInput, PlanCostNode};
use uqa_core::memory::{Produced, ProductionControl};
use uqa_sql::{
    plan::{RelationalPlan, SourcePlan},
    RowSchema, SQLError,
};

pub(super) fn capture(
    node: &PlanCostNode<'_>,
    width: Option<u64>,
    control: ProductionControl<'_>,
) -> Result<Produced<ExplainNodeEstimate>, SQLError> {
    let (kind, relation) = match node.input {
        PlanCostInput::Query(query) => (
            match query.root {
                RelationalPlan::QueryBlock(_) => "Select",
                RelationalPlan::SetOp { .. } => "SetOp",
                RelationalPlan::Values { .. } => "Values",
            },
            None,
        ),
        PlanCostInput::Command(command) => (command.name(), None),
        PlanCostInput::Source(source) => match source {
            SourcePlan::Table {
                name,
                executable_query,
                ..
            } => (
                if executable_query.is_some() {
                    "Named Query"
                } else {
                    "Relation"
                },
                Some(name.as_str()),
            ),
            SourcePlan::Join { .. } => ("Join", None),
            SourcePlan::Subquery { .. } => ("Subquery", None),
            SourcePlan::Values { .. } => ("Values Source", None),
            SourcePlan::RetainedRows(_) => ("Retained Rows", None),
            SourcePlan::Function { .. } => ("Function", None),
            SourcePlan::FunctionGroup { .. } => ("Function Group", None),
        },
    };
    let (relation, memory) = match relation {
        Some(name) => {
            let name = match uqa_core::RelationIdentity::borrowed_reference(name) {
                Some(reference) if reference.schema.is_none() => {
                    control.format(format_args!("{}", reference.name))?
                }
                _ => control.copy_text(name)?,
            };
            let (name, memory) = name.into_parts();
            (Some(name), memory)
        }
        None => (None, control.empty_reservation()),
    };
    Ok(control.finish(
        ExplainNodeEstimate {
            parent: node.parent,
            kind,
            relation,
            estimate: ExplainPlanEstimate {
                startup: node.cost.startup,
                total: node.cost.execution,
                rows: node.cost.rows,
                width,
            },
        },
        memory,
    )?)
}

pub(super) fn output_schema(input: PlanCostInput<'_>) -> Option<&RowSchema> {
    match input {
        PlanCostInput::Query(query) => query.analyzed_output.as_ref(),
        PlanCostInput::Source(
            SourcePlan::Subquery { body: query, .. }
            | SourcePlan::Table {
                executable_query: Some(query),
                ..
            },
        ) => query.analyzed_output.as_ref(),
        PlanCostInput::Source(_) | PlanCostInput::Command(_) => None,
    }
}
