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

pub(in crate::sql) fn analyze_prepared_command_schema(
    routines: &dyn RoutineResolution,
    command: &CommandPlan,
    params: &[SQLParam],
    ctes: &super::CteScope,
) -> Result<Option<RowSchema>, SQLError> {
    if command.mutation_target().is_none() {
        return Ok(None);
    }
    let mut scope = SchemaScope::for_analysis(ctes)?;
    scope.set_command_lookup_mode(command);
    let previous = scope.bind_cte_schemas(routines, command.ctes(), params, None)?;
    let result = (|| {
        let (target, expression) = scope.command_expression_schema(routines, command, params)?;
        let excluded = RowSchema::with_qualified_types(
            "excluded",
            target.columns().to_vec(),
            target.column_types().to_vec(),
        );
        let conflict_input = RowSchema::join(&expression, &excluded, std::iter::empty::<String>());
        let conflict_expressions = match command {
            CommandPlan::Insert(insert) => insert
                .on_conflict
                .as_ref()
                .map(|conflict| match &conflict.action {
                    uqa_planner::ConflictActionPlan::Update {
                        assignments,
                        predicate,
                    } => assignments
                        .iter()
                        .map(|assignment| &assignment.value)
                        .chain(predicate.as_deref())
                        .collect::<Vec<_>>(),
                    uqa_planner::ConflictActionPlan::Nothing => Vec::new(),
                })
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        if let CommandPlan::Insert(insert) = command {
            if let Some(source) = &insert.source {
                scope.bind_query(routines, source, params, None)?;
            }
        }
        for scalar in command.expressions() {
            if matches!(command, CommandPlan::Merge(_)) {
                continue;
            }
            scope.bind_expression_type(
                routines,
                scalar,
                if conflict_expressions
                    .iter()
                    .any(|candidate| std::ptr::eq(*candidate, scalar))
                {
                    &conflict_input
                } else {
                    &expression
                },
                command.scalar_subqueries(),
                params,
                None,
            )?;
        }
        if let CommandPlan::Merge(merge) = command {
            scope.bind_merge_expressions(routines, merge, params, &target, &expression)?;
        }
        let result = scope.bind_command_returning(routines, command, params)?;
        Ok(command
            .returning()
            .filter(|returning| !returning.is_empty())
            .map(|_| result))
    })();
    scope.restore_cte_schemas(previous);
    result
}

impl SchemaScope {
    pub(super) fn set_command_lookup_mode(&mut self, command: &CommandPlan) {
        let bound = match command {
            CommandPlan::Insert(plan) => plan.relations_bound,
            CommandPlan::Update(plan) => plan.relations_bound,
            CommandPlan::Delete(plan) => plan.relations_bound,
            _ => false,
        };
        self.resolution.set_lookup_mode(if bound {
            crate::engine_capabilities::RelationLookupMode::Bound
        } else {
            crate::engine_capabilities::RelationLookupMode::Dynamic
        });
    }

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
            bound_columns: None,
            name: table.to_string(),
            qualifier: command.target_qualifier().unwrap_or(table).to_string(),
            alias: None,
            column_aliases: Vec::new(),
            include_descendants: true,
        };
        // A mutation target always denotes a catalog relation, even when a CTE shadows its name.
        let previous = std::mem::take(&mut self.ctes);
        let deferred = std::mem::take(&mut self.deferred_ctes);
        let bound = match command {
            CommandPlan::Insert(plan) => plan.target_relation_bound,
            CommandPlan::Update(plan) => plan.target_relation_bound,
            CommandPlan::Delete(plan) => plan.target_relation_bound,
            _ => false,
        };
        let lookup = bound.then(|| {
            self.resolution
                .set_lookup_mode(crate::engine_capabilities::RelationLookupMode::Bound)
        });
        let result = self.bind_source(routines, &source, &[], params, None);
        if let Some(lookup) = lookup {
            self.resolution.set_lookup_mode(lookup);
        }
        self.ctes = previous;
        self.deferred_ctes = deferred;
        let target = result?;
        let columns = target
            .columns()
            .iter()
            .enumerate()
            .map(|(position, column)| target.public_name(position).unwrap_or(column).to_string())
            .collect();
        let schema = RowSchema::with_types(columns, target.column_types().to_vec());
        Ok(if target.columns_are_open(None) {
            RowSchema::with_open_columns(&schema, None)
        } else {
            schema
        })
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
        let mut expression = crate::sql::dml::returning_expression_schema(
            &target,
            command.target_qualifier().unwrap_or_default(),
            aliases,
            source.as_ref(),
        );
        if target.columns_are_open(None) {
            expression = RowSchema::with_open_columns(&expression, command.target_qualifier());
        }
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
            if let CommandPlan::Insert(insert) = command {
                if let Some(source) = &insert.source {
                    self.bind_query(routines, source, params, None)?;
                }
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
            if let Some(error) =
                crate::sql::catalog::virtual_relation_mutation_error(&self.resolution, command)
            {
                return Err(error);
            }
            Ok(RowSchema::with_types(columns, types))
        })();
        self.restore_cte_schemas(previous);
        result
    }
}
