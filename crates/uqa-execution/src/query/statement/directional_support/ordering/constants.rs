//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Literal output identities propagated through derived query boundaries.

use super::{output_expressions, CteScope, RoutineResolution, SQLError, SQLParam, ScalarExpr};
use uqa_sql::plan::{QueryPlan, RelationalPlan, SourcePlan};

pub(super) fn constant_expression<S: Clone>(
    routines: &dyn RoutineResolution,
    expression: &ScalarExpr,
    source: Option<&SourcePlan>,
    params: &[SQLParam],
    ctes: &CteScope<S>,
    control: uqa_core::memory::ProductionControl<'_>,
) -> Result<bool, SQLError> {
    Analysis {
        routines,
        params,
        control,
    }
    .expression(expression, source, ctes)
}

struct Analysis<'a> {
    routines: &'a dyn RoutineResolution,
    params: &'a [SQLParam],
    control: uqa_core::memory::ProductionControl<'a>,
}

impl Analysis<'_> {
    fn expression<S: Clone>(
        &self,
        expression: &ScalarExpr,
        source: Option<&SourcePlan>,
        ctes: &CteScope<S>,
    ) -> Result<bool, SQLError> {
        let (qualifier, column) = match expression {
            ScalarExpr::Literal(_) | ScalarExpr::TypedLiteral { .. } => return Ok(true),
            ScalarExpr::Column(column) => (None, column),
            ScalarExpr::QualifiedColumn { qualifier, column } => (Some(qualifier), column),
            _ => return Ok(false),
        };
        match source {
            Some(SourcePlan::Table {
                name,
                qualifier: source_qualifier,
                alias,
                column_aliases,
                ..
            }) => {
                if qualifier.is_some_and(|qualifier| {
                    qualifier != source_qualifier && Some(qualifier) != alias.as_ref()
                }) {
                    return Ok(false);
                }
                let Some(cte) = ctes.deferred_reference(name) else {
                    return Ok(false);
                };
                let Some(query) = cte.body.query() else {
                    return Ok(false);
                };
                let parent = ctes.definition_scope(name).expect("declared CTE scope");
                self.output(query, column, &cte.columns, column_aliases, &parent)
            }
            Some(SourcePlan::Subquery {
                body,
                alias,
                column_aliases,
                ..
            }) => {
                if qualifier.is_some_and(|qualifier| Some(qualifier) != alias.as_ref()) {
                    return Ok(false);
                }
                self.output(body, column, &[], column_aliases, ctes)
            }
            _ => Ok(false),
        }
    }

    fn output<S: Clone>(
        &self,
        query: &QueryPlan,
        column: &str,
        cte_columns: &[String],
        source_columns: &[String],
        ctes: &CteScope<S>,
    ) -> Result<bool, SQLError> {
        let RelationalPlan::QueryBlock(block) = &query.root else {
            return Ok(false);
        };
        let mut scope = std::borrow::Cow::Borrowed(ctes);
        let ordered = uqa_sql::semantics::order_cte_plans_with_control(&query.ctes, self.control)?;
        for cte in ordered.iter() {
            scope
                .to_mut()
                .insert_deferred(uqa_sql::plan::RetainedCtePlan::capture(cte, &self.control)?);
        }
        let output = output_expressions(self.routines, block, self.params, &scope)?;
        let mut found = false;
        for (index, (label, expression)) in output.iter().enumerate() {
            if source_columns
                .get(index)
                .or_else(|| cte_columns.get(index))
                .unwrap_or(label)
                != column
            {
                continue;
            }
            found = true;
            if !self.expression(expression, block.from.as_ref(), &scope)? {
                return Ok(false);
            }
        }
        Ok(found)
    }
}
