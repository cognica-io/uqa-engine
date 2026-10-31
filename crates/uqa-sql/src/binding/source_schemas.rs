//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The output columns of a `VALUES` list and of a join, as their query level sees them.

use super::scope::overlay_outer_schema_with_control;
use super::sources::JoinSchemaBinding;
use super::SchemaScope;
use crate::ast::ColumnType;
use crate::plan::QueryPlan;
use crate::routines::RoutineResolution;
use crate::semantics::{
    alias_join_schema_with_control, join_using_output_schema_with_control,
    resolve_join_using_with_control,
};
use crate::{RowSchema, SQLError, SQLParam, ScalarExpr};

#[cfg(test)]
mod production_tests;

impl SchemaScope {
    pub(super) fn bind_values_types(
        &mut self,
        routines: &dyn RoutineResolution,
        rows: &[Vec<ScalarExpr>],
        subqueries: &[QueryPlan],
        schema: Option<&RowSchema>,
        params: &[SQLParam],
    ) -> Result<Vec<Option<ColumnType>>, SQLError> {
        self.bind_values_schema(routines, rows, subqueries, schema, params)
            .map(|schema| schema.column_types().to_vec())
    }

    pub(super) fn bind_values_schema(
        &mut self,
        routines: &dyn RoutineResolution,
        rows: &[Vec<ScalarExpr>],
        subqueries: &[QueryPlan],
        schema: Option<&RowSchema>,
        params: &[SQLParam],
    ) -> Result<RowSchema, SQLError> {
        super::preparation::values_metadata(self, routines, rows, subqueries, schema, params)
    }

    pub(super) fn bind_join_output_schema(
        &mut self,
        binding: JoinSchemaBinding<'_>,
    ) -> Result<RowSchema, SQLError> {
        let JoinSchemaBinding {
            routines,
            kind,
            on,
            using,
            natural,
            alias,
            column_aliases,
            left,
            right,
            subqueries,
            params,
            outer,
        } = binding;
        if let Some(on) = on {
            let input = RowSchema::join_with_control(
                left,
                right,
                std::iter::empty::<String>(),
                &self.control(),
            )?;
            let input =
                super::with_query_table_pseudo_columns_with_control(&input, &self.control())?;
            let input = overlay_outer_schema_with_control(&input, outer, &self.control())?;
            if self.validate_references {
                self.bind_expression_type_with_control(routines, on, &input, subqueries, params)?;
                self.validate_condition(routines, on, "JOIN/ON", &input, subqueries, params)?;
            } else {
                crate::type_resolution::scalar_type_with_resolver_and_control(
                    on,
                    &input,
                    params,
                    Some(routines),
                    &self.control(),
                )?;
            }
        }
        let resolved =
            resolve_join_using_with_control(using, natural, left, right, &self.control())?;
        let schema = resolved.map_or_else(
            || {
                RowSchema::join_with_control(
                    left,
                    right,
                    std::iter::empty::<String>(),
                    &self.control(),
                )
            },
            |using| {
                join_using_output_schema_with_control(kind, left, right, &using, &self.control())
            },
        )?;
        alias_join_schema_with_control(&schema, alias, column_aliases, &self.control())
    }
}
