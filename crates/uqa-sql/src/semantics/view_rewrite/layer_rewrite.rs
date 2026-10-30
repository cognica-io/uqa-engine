//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{AutomaticViewLayer, QueryPlan, SQLError, ScalarExpr, SourcePlan, ViewRewriteContext};
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

pub(super) fn embed_layer_expression(
    services: ViewRewriteContext<'_>,
    expression: &ScalarExpr,
    layer: &AutomaticViewLayer,
    target_qualifier: &str,
    target_subqueries: &mut Vec<QueryPlan>,
) -> Result<ScalarExpr, SQLError> {
    let control = ProductionControl::uncontrolled();
    let expression = embed_owned(services, expression, layer, target_qualifier, &control)?;
    let (scalar, arena) = expression.into_parts(&control)?;
    let scalar = scalar.remap_subquery_ids(&control, &mut |id| id + target_subqueries.len())?;
    let scalar = scalar
        .finish(&control)?
        .into_uncontrolled()
        .expect("ordinary view expression");
    let queries = arena
        .finish(&control)?
        .into_uncontrolled()
        .expect("ordinary view queries");
    target_subqueries.extend(queries);
    Ok(scalar)
}

#[cfg(test)]
mod tests;
