//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Expand event whole rows in the bound SQL namespace, preserving local column and relation shadowing.

use super::{BindingContext, ScalarBindingMode, SchemaScope};
use crate::{
    catalog::events::RuleConditionBinding, plan::ExpressionPlan, routines::RoutineResolution,
    RowSchema, SQLError, ScalarExpr,
};

pub(super) fn expand(
    routines: &dyn RoutineResolution,
    context: &BindingContext,
    plan: &mut ExpressionPlan,
    binding: &RuleConditionBinding,
    table: &str,
    schema: &RowSchema,
) -> Result<(), SQLError> {
    let mut scope = SchemaScope::for_analysis(context)?;
    scope.scalar_binding = ScalarBindingMode::References;
    scope.preserve_syntax_shape = true;
    scope.stored_expression_outer = Some(schema.clone());
    scope.stored_whole_rows = ["old", "new"]
        .into_iter()
        .filter_map(|name| {
            binding
                .whole_row_expression(name, table)
                .map(|expression| (name.to_string(), expression))
        })
        .collect();
    for query in &mut plan.subqueries {
        scope.bind_query_routines_for_storage(routines, query, &[], Some(schema))?;
    }
    scope.bind_scalar_routines_for_storage(
        routines,
        &mut plan.scalar,
        schema,
        &plan.subqueries,
        &[],
    )
}

impl SchemaScope {
    pub(super) fn expand_stored_whole_rows(&self, expression: &mut ScalarExpr, schema: &RowSchema) {
        if self.stored_whole_rows.is_empty() {
            return;
        }
        let Some(outer) = &self.stored_expression_outer else {
            return;
        };
        let Some(outer_start) = schema.physical_width().checked_sub(outer.physical_width()) else {
            return;
        };
        crate::plan::rewrite_scalar_expression(expression, &mut |node| {
            let name = match node {
                ScalarExpr::Column(name) if !schema.has_unqualified_column(name) => name,
                ScalarExpr::QualifiedStar(name) => name,
                _ => return,
            };
            if schema
                .qualified_star_layout(name)
                .iter()
                .any(|(_, slot, _)| *slot < outer_start)
            {
                return;
            }
            if let Some(input) = self.stored_whole_rows.get(&name.to_ascii_lowercase()) {
                *node = input.clone();
            }
        });
    }
}
