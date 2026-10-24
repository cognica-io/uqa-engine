//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Command input analysis, mutation parameter coercion and RETURNING descriptions at preparation time.

use super::{error, ExpressionType, Preparation, QueryPlan, RowSchema, SQLError, ScalarExpr};
use crate::assignment::targets;
use crate::ast::AssignmentTarget;
use crate::plan::{
    AssignmentPlan, CommandPlan, ConflictActionPlan, ConflictPlan, InsertPlan, MergeWhenPlan,
};
use crate::semantics::sets::placement::SetPlacement;
use std::borrow::Cow;

impl Preparation<'_> {
    pub(super) fn command(&mut self, command: &CommandPlan) -> Result<Option<RowSchema>, SQLError> {
        let lookup = self.scope.resolution.lookup_mode();
        self.scope.set_command_lookup_mode(command);
        let result = (|| {
            let parameters = self.scope.routine_parameters.clone();
            let outer = parameters
                .as_ref()
                .map(crate::binding::RoutineParameterScope::schema);
            let previous = self.ctes(command.ctes(), outer)?;
            let result = self.in_query_level(|analysis| analysis.command_inner(command));
            self.scope.restore_cte_schemas(previous);
            let result = result?;
            self.parameters.collations.validate_command(command)?;
            Ok(result)
        })();
        self.scope.resolution.set_lookup_mode(lookup);
        result
    }

    fn command_inner(&mut self, command: &CommandPlan) -> Result<Option<RowSchema>, SQLError> {
        match command {
            CommandPlan::Explain { body, .. } => {
                self.plan(body)?;
                return Ok(None);
            }
            CommandPlan::CreateView { query, .. }
            | CommandPlan::CreateTableAs { query, .. }
            | CommandPlan::CreateMaterializedView { query, .. }
            | CommandPlan::DeclareCursor { query, .. } => {
                self.query(query, None)?;
                return Ok(None);
            }
            _ => {}
        }
        let parameters = self.scope.routine_parameters.clone();
        let outer = parameters
            .as_ref()
            .map(crate::binding::RoutineParameterScope::schema);
        if command.mutation_target().is_none() {
            for query in command.query_inputs() {
                self.query(query, outer)?;
            }
            return Ok(None);
        }
        let target =
            self.scope
                .bind_command_target(self.routines, command, &self.parameters.values())?;
        let preceding = crate::binding::SourceScope::command_target(command, &target);
        let source = command
            .source_input()
            .map(|source| {
                self.source_in_scope(
                    source,
                    command.scalar_subqueries(),
                    crate::binding::SourceScope {
                        outer,
                        preceding: preceding.as_ref(),
                        inherited_lateral: false,
                        retain_names: true,
                    },
                )
            })
            .transpose()?;
        let input = super::SchemaScope::command_input_schema(command, &target, source.as_ref())?;
        let input = outer.map_or(input.clone(), |outer| {
            RowSchema::with_outer_schema(&input, outer)
        });
        let subqueries = command.scalar_subqueries();
        match command {
            CommandPlan::Insert(insert) => {
                self.insert(insert, &target, &input, outer, subqueries)?;
            }
            CommandPlan::Update(update) => {
                if let Some(predicate) = &update.predicate {
                    self.placed_condition(
                        SetPlacement::Where,
                        predicate,
                        &input,
                        subqueries,
                        "WHERE",
                    )?;
                }
                let result = self.command_result(command, &target, &input, source.as_ref())?;
                self.in_placement(SetPlacement::Update, |analysis| {
                    analysis.assignments(
                        &update.table,
                        &update.assignments,
                        &target,
                        &input,
                        subqueries,
                    )
                })?;
                return Ok(result);
            }
            CommandPlan::Delete(delete) => {
                if let Some(predicate) = &delete.predicate {
                    self.placed_condition(
                        SetPlacement::Where,
                        predicate,
                        &input,
                        subqueries,
                        "WHERE",
                    )?;
                }
            }
            CommandPlan::Merge(merge) => {
                self.placed_condition(
                    SetPlacement::JoinCondition,
                    &merge.join_condition,
                    &input,
                    subqueries,
                    "JOIN/ON",
                )?;
                let result = self.command_result(command, &target, &input, source.as_ref())?;
                self.merge(merge, &target, &input, subqueries)?;
                return Ok(result);
            }
            _ => {}
        }
        self.command_result(command, &target, &input, source.as_ref())
    }

    /// Analyze an INSERT's VALUES rows or source query and its ON CONFLICT clause.
    fn insert(
        &mut self,
        insert: &InsertPlan,
        target: &RowSchema,
        input: &RowSchema,
        outer: Option<&RowSchema>,
        subqueries: &[QueryPlan],
    ) -> Result<(), SQLError> {
        targets::validate_repeated_targets(&insert.columns, true)?;
        validate_target_columns(&insert.table, insert.columns.iter(), target)?;
        let values_input = outer.cloned().unwrap_or_default();
        // A single VALUES row is processed like a target list, so its set-returning calls expand rows; a multiple-row list admits none.
        let placement = if insert.rows.len() > 1 {
            SetPlacement::Values
        } else {
            SetPlacement::Expanding
        };
        for row in &insert.rows {
            self.insert_row(
                row,
                placement,
                &insert.columns,
                target,
                &values_input,
                subqueries,
            )?;
        }
        if let Some(source) = &insert.source {
            let mut output = self.query_output(source, outer, true)?;
            self.insert_values(
                &mut output.types,
                &insert.columns,
                target,
                input,
                subqueries,
            )?;
        }
        if let Some(conflict) = &insert.on_conflict {
            self.conflict(&insert.table, conflict, target, input, subqueries)?;
        }
        Ok(())
    }

    /// Analyze an ON CONFLICT clause: its inference expressions and predicate as index expressions and predicates, then a DO UPDATE action as an UPDATE.
    fn conflict(
        &mut self,
        table: &str,
        conflict: &ConflictPlan,
        target: &RowSchema,
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<(), SQLError> {
        self.in_placement(SetPlacement::IndexExpression, |analysis| {
            for expression in &conflict.expressions {
                analysis.expression(expression, input, subqueries)?;
            }
            Ok(())
        })?;
        if let Some(predicate) = &conflict.predicate {
            self.in_placement(SetPlacement::IndexPredicate, |analysis| {
                analysis.expression(predicate, input, subqueries)
            })?;
        }
        if let ConflictActionPlan::Update {
            assignments,
            predicate,
        } = &conflict.action
        {
            let excluded = RowSchema::with_relation_qualifier(target, "excluded");
            let conflict_input = RowSchema::join(input, &excluded, std::iter::empty::<String>());
            self.in_placement(SetPlacement::Update, |analysis| {
                analysis.assignments(table, assignments, target, &conflict_input, subqueries)
            })?;
            if let Some(predicate) = predicate {
                self.placed_condition(
                    SetPlacement::Where,
                    predicate,
                    &conflict_input,
                    subqueries,
                    "WHERE",
                )?;
            }
        }
        Ok(())
    }

    /// Analyze a condition `construct` requires to be boolean as the expression kind `placement`.
    fn placed_condition(
        &mut self,
        placement: SetPlacement,
        condition: &ScalarExpr,
        input: &RowSchema,
        subqueries: &[QueryPlan],
        construct: &str,
    ) -> Result<(), SQLError> {
        self.in_placement(placement, |analysis| {
            analysis.require_boolean(condition, input, subqueries, construct)
        })
    }

    fn command_result(
        &mut self,
        command: &CommandPlan,
        target: &RowSchema,
        input: &RowSchema,
        source: Option<&RowSchema>,
    ) -> Result<Option<RowSchema>, SQLError> {
        let returning = command.returning().unwrap_or_default();
        if returning.is_empty() {
            return Ok(None);
        }
        let returning =
            crate::semantics::returning::command_returning_projections(command, target)?;
        let star =
            crate::semantics::returning::command_returning_star_schema(command, target, source)?;
        let previous = self.scope.merge_returning;
        self.scope.merge_returning |= matches!(command, CommandPlan::Merge(_));
        let result = self.in_placement(SetPlacement::Returning, |analysis| {
            analysis.projections(
                returning.iter().map(AsRef::as_ref),
                &star,
                input,
                command.scalar_subqueries(),
            )
        });
        self.scope.merge_returning = previous;
        let mut output = result?;
        crate::semantics::returning::validate_returning_width(
            command.returning().unwrap_or_default(),
            output.columns.len(),
        )?;
        self.resolve_targets(&mut output)?;
        Ok(Some(output.schema()))
    }

    fn merge(
        &mut self,
        merge: &crate::plan::MergePlan,
        target: &RowSchema,
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<(), SQLError> {
        if let Some(predicate) = &merge.target_predicate {
            self.placed_condition(SetPlacement::Where, predicate, input, subqueries, "WHERE")?;
        }
        let parameters = self.scope.routine_parameters.clone();
        let outer = parameters
            .as_ref()
            .map(crate::binding::RoutineParameterScope::schema);
        let target_qualifier = merge
            .target_alias
            .as_deref()
            .unwrap_or(&merge.target_qualifier);
        let target_input = RowSchema::with_relation_qualifier(target, target_qualifier);
        let target_input = crate::binding::overlay_outer_schema(&target_input, outer);
        let source_input = self.source(&merge.source, subqueries, outer)?;
        let source_input = crate::binding::overlay_outer_schema(&source_input, outer);
        for clause in &merge.when_clauses {
            let input = match clause {
                MergeWhenPlan::UpdateNotMatchedBySource { .. }
                | MergeWhenPlan::DeleteNotMatchedBySource { .. }
                | MergeWhenPlan::NothingNotMatchedBySource { .. } => &target_input,
                MergeWhenPlan::InsertNotMatched { .. }
                | MergeWhenPlan::NothingNotMatched { .. } => &source_input,
                _ => input,
            };
            let condition = match clause {
                MergeWhenPlan::UpdateMatched { condition, .. }
                | MergeWhenPlan::UpdateNotMatchedBySource { condition, .. }
                | MergeWhenPlan::DeleteMatched { condition }
                | MergeWhenPlan::DeleteNotMatchedBySource { condition }
                | MergeWhenPlan::InsertNotMatched { condition, .. }
                | MergeWhenPlan::NothingMatched { condition }
                | MergeWhenPlan::NothingNotMatched { condition }
                | MergeWhenPlan::NothingNotMatchedBySource { condition } => condition,
            };
            if let Some(condition) = condition {
                self.placed_condition(
                    SetPlacement::MergeWhen,
                    condition,
                    input,
                    subqueries,
                    "WHEN",
                )?;
            }
            match clause {
                MergeWhenPlan::UpdateMatched { assignments, .. }
                | MergeWhenPlan::UpdateNotMatchedBySource { assignments, .. } => {
                    self.in_placement(SetPlacement::Update, |analysis| {
                        analysis.assignments(&merge.target, assignments, target, input, subqueries)
                    })?;
                }
                MergeWhenPlan::InsertNotMatched {
                    columns, values, ..
                } => {
                    validate_target_columns(&merge.target, columns.iter(), target)?;
                    self.insert_row(
                        values,
                        SetPlacement::Expanding,
                        columns,
                        target,
                        input,
                        subqueries,
                    )?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn assignments(
        &mut self,
        table: &str,
        assignments: &[AssignmentPlan],
        target: &RowSchema,
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<(), SQLError> {
        let mut values = Vec::with_capacity(assignments.len());
        for assignment in assignments {
            values.push(match &assignment.target {
                crate::ast::AssignmentTargets::Single(_) => {
                    vec![self.expression(&assignment.value, input, subqueries)?]
                }
                crate::ast::AssignmentTargets::Multiple(targets) => {
                    let ScalarExpr::ScalarSubquery(index) = &assignment.value else {
                        return Err(error(
                            "XX000",
                            "multiple-column assignment lost its query source".into(),
                        ));
                    };
                    let query = subqueries.get(*index).ok_or_else(|| {
                        error(
                            "XX000",
                            "assignment subquery slot is outside its plan".into(),
                        )
                    })?;
                    let output = self.query(query, Some(input))?;
                    if targets.source_width != output.columns().len() {
                        return Err(error(
                            "42601",
                            "number of columns does not match number of values".into(),
                        ));
                    }
                    targets
                        .source_positions
                        .iter()
                        .copied()
                        .map(|position| {
                            ExpressionType::resolved(output.column_type(position).cloned())
                        })
                        .collect()
                }
            });
        }
        validate_target_columns(
            table,
            assignments
                .iter()
                .flat_map(|assignment| assignment.target.targets()),
            target,
        )?;
        targets::validate_repeated_targets(
            assignments
                .iter()
                .flat_map(|assignment| assignment.target.targets()),
            false,
        )?;
        for (assignment, values) in assignments.iter().zip(&mut values) {
            for (column, value) in assignment.target.targets().iter().zip(values) {
                if matches!(assignment.value, ScalarExpr::Default) {
                    targets::validate_assignment_default(column)?;
                }
                self.assignment(value, column, target, input, subqueries)?;
            }
        }
        Ok(())
    }

    /// Analyze one VALUES row as `placement`, then its target columns, whose subscripts may expand rows as INSERT targets do.
    fn insert_row(
        &mut self,
        row: &[ScalarExpr],
        placement: SetPlacement,
        columns: &[crate::ast::AssignmentTarget<ScalarExpr>],
        target: &RowSchema,
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<(), SQLError> {
        let mut values = self.in_placement(placement, |analysis| {
            row.iter()
                .map(|expression| analysis.expression(expression, input, subqueries))
                .collect::<Result<Vec<_>, _>>()
        })?;
        for (expression, target) in row.iter().zip(columns) {
            if matches!(expression, ScalarExpr::Default) {
                targets::validate_assignment_default(target)?;
            }
        }
        self.insert_values(&mut values, columns, target, input, subqueries)
    }

    fn insert_values(
        &mut self,
        values: &mut [ExpressionType],
        columns: &[crate::ast::AssignmentTarget<ScalarExpr>],
        target: &RowSchema,
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<(), SQLError> {
        targets::validate_repeated_targets(columns, true)?;
        let names: Cow<'_, [AssignmentTarget<ScalarExpr>]> = if columns.is_empty() {
            Cow::Owned(
                target
                    .columns()
                    .iter()
                    .cloned()
                    .map(AssignmentTarget::from)
                    .collect::<Vec<_>>(),
            )
        } else {
            Cow::Borrowed(columns)
        };
        if values.len() > names.len() {
            return Err(error(
                "42601",
                "INSERT has more expressions than target columns".into(),
            ));
        }
        if !columns.is_empty() && values.len() < columns.len() {
            return Err(error(
                "42601",
                "INSERT has more target columns than expressions".into(),
            ));
        }
        for (value, column) in values.iter_mut().zip(names.iter()) {
            self.assignment(value, column, target, input, subqueries)?;
        }
        Ok(())
    }

    fn assignment(
        &mut self,
        value: &mut ExpressionType,
        assignment: &AssignmentTarget<ScalarExpr>,
        target: &RowSchema,
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<(), SQLError> {
        let column = &assignment.column;
        if target.columns_are_open(None) && target.unqualified_position(column).is_none() {
            return targets::validate_assignment_type(assignment, None);
        }
        let index = target
            .columns()
            .iter()
            .position(|name| name == column)
            .ok_or_else(|| error("42703", format!("column \"{column}\" does not exist")))?;
        let Some(declared) = target.column_type(index) else {
            return targets::validate_assignment_type(assignment, None);
        };
        let required = if targets::has_field_step(assignment) {
            targets::field_assignment_types(
                assignment,
                declared,
                crate::type_resolution::FunctionTypeResolver::composite_types(self.routines),
            )?
            .pop()
            .ok_or_else(|| SQLError::Internal("field assignment has no value type".into()))?
        } else {
            targets::assignment_value_type(assignment, declared)?
        };
        for bound in assignment.expressions() {
            let mut ty = self.expression(bound, input, subqueries)?;
            self.parameters
                .coerce_unknown(&mut ty, &crate::ColumnType::Integer)?;
            if ty.ty.as_ref().is_some_and(|ty| {
                !crate::assignment_type_compatible(ty, &crate::ColumnType::Integer)
            }) {
                return Err(error(
                    "42804",
                    "array subscript must have type integer".into(),
                ));
            }
            if let ScalarExpr::Literal(value @ uqa_core::Value::Str(_)) = bound {
                crate::expr::cast_value(value, "integer")?;
            }
        }
        self.parameters.coerce_unknown(value, &required)?;
        targets::validate_assignment_source(assignment, &required, value.ty.as_ref())?;
        targets::validate_assignment_result(assignment, declared)
    }
}

fn validate_target_columns<'a>(
    table: &str,
    columns: impl Iterator<Item = &'a AssignmentTarget<ScalarExpr>>,
    target: &RowSchema,
) -> Result<(), SQLError> {
    if target.columns_are_open(None) {
        return Ok(());
    }
    for assignment in columns {
        let column = &assignment.column;
        if !target.columns().contains(column) {
            let identity =
                crate::RelationIdentity::from_legacy_name(table).map_err(SQLError::Internal)?;
            return Err(error(
                "42703",
                format!(
                    "column \"{column}\" of relation \"{}\" does not exist",
                    identity.name
                ),
            ));
        }
    }
    Ok(())
}
