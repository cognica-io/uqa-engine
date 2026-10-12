//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The output columns of a `VALUES` list and of a join, as their query level sees them.

use super::scope::overlay_outer_schema;
use super::sources::JoinSchemaBinding;
use super::SchemaScope;
use crate::ast::ColumnType;
use crate::plan::QueryPlan;
use crate::routines::RoutineResolution;
use crate::semantics::{alias_join_schema, join_using_output_schema, resolve_join_using};
use crate::{RowSchema, SQLError, SQLParam, ScalarExpr};

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
            let input = RowSchema::join(left, right, std::iter::empty::<String>());
            let input = overlay_outer_schema(&input, outer);
            if self.validate_references {
                self.bind_expression_type(routines, on, &input, subqueries, params)?;
                self.validate_condition(routines, on, "JOIN/ON", &input, subqueries, params)?;
            } else {
                crate::scalar_type_with_resolver(on, &input, params, routines)?;
            }
        }
        let resolved = resolve_join_using(using, natural, left, right)?;
        let schema = resolved.map_or_else(
            || Ok(RowSchema::join(left, right, std::iter::empty())),
            |using| join_using_output_schema(kind, left, right, &using),
        )?;
        alias_join_schema(&schema, alias, column_aliases)
    }
}
