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
    scope.stored_whole_rows = [
        binding.whole_row_expression_with_control("old", table, &scope.control())?,
        binding.whole_row_expression_with_control("new", table, &scope.control())?,
    ];
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
    pub(super) fn stored_whole_row<'a>(
        &'a self,
        expression: &ScalarExpr,
        schema: &RowSchema,
    ) -> Option<&'a crate::plan::OwnedScalarExpr> {
        let outer = self.stored_expression_outer.as_ref()?;
        let boundary = schema
            .physical_width()
            .checked_sub(outer.physical_width())?;
        let name = match expression {
            ScalarExpr::Column(name) if !schema.has_unqualified_column(name) => name,
            ScalarExpr::QualifiedStar(name) => name,
            _ => return None,
        };
        if schema.has_qualified_slot_before(name, boundary) {
            return None;
        }
        let index = if name.eq_ignore_ascii_case("old") {
            0
        } else if name.eq_ignore_ascii_case("new") {
            1
        } else {
            return None;
        };
        self.stored_whole_rows[index].as_ref()
    }

    pub(super) fn expand_stored_whole_rows(&self, expression: &mut ScalarExpr, schema: &RowSchema) {
        if self.stored_whole_rows.iter().all(Option::is_none) {
            return;
        }
        crate::plan::rewrite_scalar_expression(expression, &mut |node| {
            if let Some(input) = self.stored_whole_row(node, schema) {
                *node = (**input).clone();
            }
        });
    }
}
