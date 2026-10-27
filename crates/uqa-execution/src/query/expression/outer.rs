//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain a predicate's lexical outer row independently of its local input rows.

use crate::{
    ExecResult, ExpressionEvaluator, OwnedPhysicalRow, PhysicalRow, RowSchema, ScalarExpr,
    SharedExpressionEvaluator,
};
use std::borrow::Cow;
use std::sync::{Arc, OnceLock};
use uqa_core::Value;
use uqa_sql::{expr::RowLookup, ColumnType, SQLError, SQLParam};

#[cfg(test)]
mod tests;

pub(crate) fn with_outer_row<'a>(
    evaluator: SharedExpressionEvaluator<'a>,
    outer: Option<&OwnedPhysicalRow>,
) -> SharedExpressionEvaluator<'a> {
    outer.map_or_else(
        || evaluator.clone(),
        |outer| {
            Arc::new(OuterExpressionEvaluator {
                evaluator: evaluator.clone(),
                outer: outer.clone(),
                physical_schemas: std::array::from_fn(|_| OnceLock::new()),
            })
        },
    )
}

struct OuterExpressionEvaluator<'a> {
    evaluator: SharedExpressionEvaluator<'a>,
    outer: OwnedPhysicalRow,
    /// A join evaluates keys in each input layout and its predicate in the joined layout. Additional callers compose an ephemeral schema instead of retaining an unbounded collection.
    physical_schemas: [OnceLock<(RowSchema, RowSchema)>; 3],
}

impl OuterExpressionEvaluator<'_> {
    fn physical_schema<'a>(&'a self, local: &'a RowSchema) -> Cow<'a, RowSchema> {
        for entry in &self.physical_schemas {
            let (input, combined) = entry.get_or_init(|| {
                (
                    local.clone(),
                    RowSchema::with_outer_schema(local, &self.outer.schema),
                )
            });
            if input.shares_index(local) {
                return Cow::Borrowed(combined);
            }
        }
        Cow::Owned(RowSchema::with_outer_schema(local, &self.outer.schema))
    }
}

impl ExpressionEvaluator for OuterExpressionEvaluator<'_> {
    fn evaluate(&self, expression: &ScalarExpr, row: &dyn RowLookup) -> ExecResult<Value> {
        self.evaluator.evaluate(
            expression,
            &OuterLookup {
                local: row,
                outer: &self.outer,
            },
        )
    }

    fn evaluate_physical(
        &self,
        expression: &ScalarExpr,
        schema: &RowSchema,
        row: &PhysicalRow,
    ) -> ExecResult<Value> {
        self.evaluator.evaluate_physical(
            expression,
            &self.physical_schema(schema),
            &PhysicalRow::concat(row, &self.outer.row),
        )
    }

    fn evaluate_physical_with_function_states(
        &self,
        expression: &ScalarExpr,
        schema: &RowSchema,
        row: &PhysicalRow,
        states: &crate::scalar::FunctionCallStates,
    ) -> ExecResult<Value> {
        self.evaluator.evaluate_physical_with_function_states(
            expression,
            &self.physical_schema(schema),
            &PhysicalRow::concat(row, &self.outer.row),
            states,
        )
    }

    fn enum_labels(&self) -> Option<&dyn uqa_sql::expr::enums::EnumLabelCatalog> {
        self.evaluator.enum_labels()
    }

    fn parameters(&self) -> &[SQLParam] {
        self.evaluator.parameters()
    }

    fn expression_type(
        &self,
        expression: &ScalarExpr,
        schema: &RowSchema,
    ) -> Result<Option<ColumnType>, SQLError> {
        self.evaluator.expression_type(
            expression,
            &RowSchema::with_outer_schema(schema, &self.outer.schema),
        )
    }

    fn bind_type_introspection(&self, expression: ScalarExpr, schema: &RowSchema) -> ScalarExpr {
        self.evaluator.bind_type_introspection(
            expression,
            &RowSchema::with_outer_schema(schema, &self.outer.schema),
        )
    }

    fn star_column_visible(&self, column: &str) -> bool {
        self.evaluator.star_column_visible(column)
    }

    fn star_position_visible(&self, schema: &RowSchema, position: usize) -> bool {
        self.evaluator.star_position_visible(schema, position)
    }

    fn project_star(&self, row: &dyn RowLookup) -> ExecResult<Vec<(String, Value)>> {
        self.evaluator.project_star(row)
    }
}

/// Named callers have independent key spaces, so this borrowed lookup deliberately does not retain positional keys. Physical callers above use the composed schema and shared value fragments.
struct OuterLookup<'a> {
    local: &'a dyn RowLookup,
    outer: &'a OwnedPhysicalRow,
}

impl OuterLookup<'_> {
    fn local_column(&self, name: &str) -> bool {
        self.local.column(name).is_some()
            || self.local.column_is_ambiguous(name)
            || self
                .local
                .resolution_schema()
                .is_some_and(|schema| schema.has_unqualified_column(name))
    }

    fn local_qualifier(&self, qualifier: &str, column: &str) -> bool {
        self.local.qualified_column(qualifier, column).is_some()
            || self.local.qualified_column_is_ambiguous(qualifier, column)
            || self
                .local
                .resolution_schema()
                .is_some_and(|schema| schema.has_qualifier(qualifier))
    }
}

impl RowLookup for OuterLookup<'_> {
    fn column(&self, name: &str) -> Option<&Value> {
        if self.local_column(name) {
            self.local.column(name)
        } else {
            self.outer.column(name)
        }
    }

    fn column_is_ambiguous(&self, name: &str) -> bool {
        if self.local_column(name) {
            self.local.column_is_ambiguous(name)
        } else {
            self.outer.column_is_ambiguous(name)
        }
    }

    fn qualified_column(&self, qualifier: &str, column: &str) -> Option<&Value> {
        if self.local_qualifier(qualifier, column) {
            self.local.qualified_column(qualifier, column)
        } else {
            self.outer.qualified_column(qualifier, column)
        }
    }

    fn qualified_column_is_ambiguous(&self, qualifier: &str, column: &str) -> bool {
        if self.local_qualifier(qualifier, column) {
            self.local.qualified_column_is_ambiguous(qualifier, column)
        } else {
            self.outer.qualified_column_is_ambiguous(qualifier, column)
        }
    }

    fn positional_column(&self, index: usize) -> Option<&Value> {
        self.local.positional_column(index)
    }

    fn internal_column(&self, column: uqa_sql::ast::InternalColumnRef) -> Option<&Value> {
        self.local
            .internal_column(column)
            .or_else(|| self.outer.internal_column(column))
    }

    fn score_source(&self, qualifier: Option<&str>) -> Option<&Value> {
        if self.local.score_source_is_ambiguous(qualifier) {
            None
        } else {
            self.local
                .score_source(qualifier)
                .or_else(|| self.outer.score_source(qualifier))
        }
    }

    fn score_source_is_ambiguous(&self, qualifier: Option<&str>) -> bool {
        self.local.score_source_is_ambiguous(qualifier)
            || self.local.score_source(qualifier).is_none()
                && self.outer.score_source_is_ambiguous(qualifier)
    }

    fn visit_columns(&self, visitor: &mut dyn FnMut(&str, &Value)) {
        self.local.visit_columns(visitor);
    }
}
