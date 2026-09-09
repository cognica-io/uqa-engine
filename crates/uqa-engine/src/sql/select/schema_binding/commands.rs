//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Static RETURNING schemas for command CTEs.

use super::{
    projection_columns, projection_star_columns, ColumnType, RowSchema, SQLError, SQLParam,
    ScalarExpr, SchemaScope, SourcePlan,
};
use crate::engine_user_functions::RoutineResolution;
use uqa_planner::{CommandPlan, CtePlanBody};

impl SchemaScope {
    pub(super) fn bind_cte_body(
        &mut self,
        routines: &dyn RoutineResolution,
        body: &CtePlanBody,
        params: &[SQLParam],
        outer: Option<&RowSchema>,
    ) -> Result<RowSchema, SQLError> {
        match body {
            CtePlanBody::Query(query) => self.bind_query(routines, query, params, outer),
            CtePlanBody::Command(command) => self.bind_command_returning(routines, command, params),
        }
    }

    fn bind_command_target(
        &mut self,
        routines: &dyn RoutineResolution,
        command: &CommandPlan,
        params: &[SQLParam],
    ) -> Result<RowSchema, SQLError> {
        let table = command
            .mutation_target()
            .ok_or_else(|| SQLError::Internal("non-DML command in a WITH definition".into()))?;
        let source = SourcePlan::Table {
            name: table.to_string(),
            qualifier: command.target_qualifier().unwrap_or(table).to_string(),
            alias: None,
            column_aliases: Vec::new(),
            include_descendants: true,
        };
        // A mutation target always denotes a catalog relation, even when a CTE shadows its name.
        let previous = std::mem::take(&mut self.ctes);
        let deferred = std::mem::take(&mut self.deferred_ctes);
        let result = self.bind_source(routines, &source, &[], params, None);
        self.ctes = previous;
        self.deferred_ctes = deferred;
        let target = result?;
        let columns = target
            .columns()
            .iter()
            .enumerate()
            .map(|(position, column)| target.public_name(position).unwrap_or(column).to_string())
            .collect();
        Ok(RowSchema::with_types(
            columns,
            target.column_types().to_vec(),
        ))
    }

    pub(super) fn command_expression_schema(
        &mut self,
        routines: &dyn RoutineResolution,
        command: &CommandPlan,
        params: &[SQLParam],
    ) -> Result<(RowSchema, RowSchema), SQLError> {
        let target = self.bind_command_target(routines, command, params)?;
        let source = command
            .source_input()
            .map(|source| {
                self.bind_source(routines, source, command.scalar_subqueries(), params, None)
            })
            .transpose()?;
        let aliases = command
            .returning_aliases()
            .ok_or_else(|| SQLError::Internal("command CTE has no RETURNING namespace".into()))?;
        let expression = crate::sql::dml::returning_expression_schema(
            &target,
            command.target_qualifier().unwrap_or_default(),
            aliases,
            source.as_ref(),
        );
        Ok((target, expression))
    }

    pub(super) fn bind_command_returning(
        &mut self,
        routines: &dyn RoutineResolution,
        command: &CommandPlan,
        params: &[SQLParam],
    ) -> Result<RowSchema, SQLError> {
        let previous = self.bind_cte_schemas(routines, command.ctes(), params, None)?;
        let result = (|| {
            let (target, expression) = self.command_expression_schema(routines, command, params)?;
            for query in command.query_inputs() {
                self.bind_query(routines, query, params, Some(&expression))?;
            }
            let returning = command.returning().unwrap_or_default();
            let labels = projection_columns(returning);
            let mut columns = Vec::new();
            let mut types: Vec<Option<ColumnType>> = Vec::new();
            for (position, projection) in returning.iter().enumerate() {
                let expansion = if matches!(projection.expr, ScalarExpr::QualifiedStar(_)) {
                    &expression
                } else {
                    &target
                };
                if let Some(star) = projection_star_columns(&projection.expr, expansion)? {
                    for (column, ty) in star {
                        columns.push(column);
                        types.push(ty);
                    }
                } else {
                    columns.push(labels[position].clone());
                    types.push(self.bind_expression_type(
                        routines,
                        &projection.expr,
                        &expression,
                        command.scalar_subqueries(),
                        params,
                        Some(&expression),
                    )?);
                }
            }
            Ok(RowSchema::with_types(columns, types))
        })();
        self.restore_cte_schemas(previous);
        result
    }
}
