//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::SourcePlan;
#[cfg(test)]
use super::{QueryPlan, ScalarExpr};
#[cfg(test)]
use uqa_core::memory::ProductionControl;

mod owned;
mod qualifiers;
mod renaming;
mod scalars;
pub(super) use owned::embed as embed_owned;

fn source_plan_declares_qualifier(source: &SourcePlan, qualifier: &str) -> bool {
    match source {
        crate::plan::SourcePlan::RetainedRows(_) => false,
        SourcePlan::Table {
            qualifier: source_qualifier,
            alias,
            ..
        } => alias.as_deref().unwrap_or(source_qualifier) == qualifier,
        SourcePlan::Join {
            left, right, alias, ..
        } => {
            alias.as_deref() == Some(qualifier)
                || source_plan_declares_qualifier(left, qualifier)
                || source_plan_declares_qualifier(right, qualifier)
        }
        SourcePlan::Values { alias, .. } | SourcePlan::Subquery { alias, .. } => {
            alias.as_deref() == Some(qualifier)
        }
        SourcePlan::Function {
            output_name, alias, ..
        } => alias.as_deref().unwrap_or(output_name) == qualifier,
        SourcePlan::FunctionGroup {
            functions, alias, ..
        } => {
            alias.as_deref().or_else(|| {
                functions
                    .first()
                    .map(|function| function.output_name.as_str())
            }) == Some(qualifier)
        }
    }
}

#[cfg(test)]
fn fresh_query_qualifier(query: &QueryPlan) -> String {
    qualifiers::fresh(query, &ProductionControl::uncontrolled())
        .expect("ordinary qualifier production")
        .into_uncontrolled()
        .expect("ordinary qualifier ownership")
}

#[cfg(test)]
mod tests;
