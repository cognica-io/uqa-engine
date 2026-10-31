//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Static RETURNING schemas for command CTEs.

use super::{RowSchema, SQLError, SQLParam, ScalarExpr, SchemaScope, SourcePlan};
use crate::plan::{CommandPlan, CtePlanBody};
use crate::routines::RoutineResolution;

pub fn analyze_prepared_command_schema(
    routines: &dyn RoutineResolution,
    command: &CommandPlan,
    params: &[SQLParam],
    ctes: &super::BindingContext,
) -> Result<Option<RowSchema>, SQLError> {
    analyze_prepared_command_schema_with_control(
        routines,
        command,
        params,
        ctes.copy_with_control(&uqa_core::memory::ProductionControl::uncontrolled())?,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
}

pub fn analyze_prepared_command_schema_with_control(
    routines: &dyn RoutineResolution,
    command: &CommandPlan,
    params: &[SQLParam],
    ctes: super::production::ControlledBindingContext<'_>,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<Option<RowSchema>, SQLError> {
    control.check()?;
    if let CommandPlan::Explain { options, .. } = command {
        return options.result_schema_with_control(control).map(Some);
    }
    if command.mutation_target().is_none() {
        return Ok(None);
    }
    let mut scope = SchemaScope::for_analysis_with_control(ctes, control)?;
    scope.set_command_lookup_mode(command);
    let previous = scope.bind_cte_schemas(routines, command.ctes(), params, None)?;
    let result = (|| {
        let (target, expression, _) =
            scope.command_expression_schema(routines, command, params, None)?;
        let excluded =
            RowSchema::with_relation_qualifier_with_control(&target, "excluded", control)?;
        let conflict_input = RowSchema::join_with_control(
            &expression,
            &excluded,
            std::iter::empty::<String>(),
            control,
        )?;
        let is_conflict_expression = |scalar: &ScalarExpr| {
            let CommandPlan::Insert(insert) = command else {
                return false;
            };
            let Some(conflict) = &insert.on_conflict else {
                return false;
            };
            let crate::plan::ConflictActionPlan::Update {
                assignments,
                predicate,
            } = &conflict.action
            else {
                return false;
            };
            assignments
                .iter()
                .flat_map(crate::plan::AssignmentPlan::expressions)
                .chain(predicate.as_deref())
                .any(|candidate| std::ptr::eq(candidate, scalar))
        };
        if let CommandPlan::Insert(insert) = command {
            if let Some(source) = &insert.source {
                scope.bind_query(routines, source, params, None)?;
            }
        }
        for scalar in command.expressions() {
            control.check()?;
            if matches!(command, CommandPlan::Merge(_)) {
                continue;
            }
            scope.bind_expression_type(
                routines,
                scalar,
                if is_conflict_expression(scalar) {
                    &conflict_input
                } else {
                    &expression
                },
                command.scalar_subqueries(),
                params,
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
            crate::catalog::resolution::RelationLookupMode::Bound
        } else {
            crate::catalog::resolution::RelationLookupMode::Dynamic
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

    pub(super) fn bind_command_target(
        &mut self,
        routines: &dyn RoutineResolution,
        command: &CommandPlan,
        params: &[SQLParam],
    ) -> Result<RowSchema, SQLError> {
        let table = command
            .mutation_target()
            .ok_or_else(|| SQLError::Internal("non-DML command in a WITH definition".into()))?;
        let source = SourcePlan::Table {
            executable_query: None,
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
                .set_lookup_mode(crate::catalog::resolution::RelationLookupMode::Bound)
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
        let schema = RowSchema::with_types(columns, target.column_types().to_vec())
            .with_column_collations(
                (0..target.len()).map(|position| target.column_collation(position)),
            );
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
        outer: Option<&RowSchema>,
    ) -> Result<(RowSchema, RowSchema, RowSchema), SQLError> {
        let target = self.bind_command_target(routines, command, params)?;
        let preceding = super::SourceScope::command_target(command, &target);
        let source = command
            .source_input()
            .map(|source| {
                self.bind_source_in_scope(
                    routines,
                    source,
                    command.scalar_subqueries(),
                    params,
                    super::SourceScope {
                        outer,
                        preceding: preceding.as_ref(),
                        inherited_lateral: false,
                        retain_names: true,
                    },
                )
            })
            .transpose()?;
        let expression = Self::command_input_schema(command, &target, source.as_ref())?;
        let star = crate::semantics::returning::command_returning_star_schema(
            command,
            &target,
            source.as_ref(),
        )?;
        Ok((target, expression, star))
    }

    pub(super) fn command_input_schema(
        command: &CommandPlan,
        target: &RowSchema,
        source: Option<&RowSchema>,
    ) -> Result<RowSchema, SQLError> {
        let aliases = command
            .returning_aliases()
            .ok_or_else(|| SQLError::Internal("command CTE has no RETURNING namespace".into()))?;
        let mut expression = crate::semantics::returning_expression_schema(
            target,
            command.target_qualifier().unwrap_or_default(),
            aliases,
            source,
        );
        if target.columns_are_open(None) {
            expression = RowSchema::with_open_columns(&expression, command.target_qualifier());
        }
        Ok(expression)
    }

    pub(super) fn bind_command_returning(
        &mut self,
        routines: &dyn RoutineResolution,
        command: &CommandPlan,
        params: &[SQLParam],
    ) -> Result<RowSchema, SQLError> {
        let previous = self.bind_cte_schemas(routines, command.ctes(), params, None)?;
        let result = (|| {
            let (target, expression, star) =
                self.command_expression_schema(routines, command, params, None)?;
            if let CommandPlan::Insert(insert) = command {
                if let Some(source) = &insert.source {
                    self.bind_query(routines, source, params, None)?;
                }
            }
            let returning =
                crate::semantics::returning::command_returning_projections(command, &target)?;
            let owner = self.control().capture();
            let control = owner.control();
            let mut output = super::projection::ProjectionOutput::new(control);
            for projection in &returning {
                let expansion = if matches!(projection.expr, ScalarExpr::QualifiedStar(_)) {
                    &expression
                } else {
                    &star
                };
                if !output.star(&projection.expr, expansion)? {
                    let metadata = self.bind_projection_metadata(
                        routines,
                        &projection.expr,
                        &expression,
                        command.scalar_subqueries(),
                        params,
                    )?;
                    crate::semantics::with_projection_label(projection, &control, |label| {
                        output.push(label, metadata)
                    })?;
                }
            }
            crate::semantics::returning::validate_returning_width(
                command.returning().unwrap_or_default(),
                output.len(),
            )?;
            if let Some(error) = crate::semantics::virtual_relation_mutation_error(
                self.catalog.as_ref(),
                &self.resolution,
                command,
            )? {
                return Err(error);
            }
            for projection in &returning {
                self.collations.validate_expression(&projection.expr)?;
            }
            output.finish()
        })();
        self.restore_cte_schemas(previous);
        result
    }
}
