//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Mutation parameter coercion and RETURNING descriptions at preparation time.

use super::{error, ExpressionType, Preparation, QueryPlan, RowSchema, SQLError, ScalarExpr};
use crate::plan::{AssignmentPlan, CommandPlan, ConflictActionPlan, MergeWhenPlan};

impl Preparation<'_> {
    pub(super) fn command(&mut self, command: &CommandPlan) -> Result<Option<RowSchema>, SQLError> {
        let lookup = self.scope.resolution.lookup_mode();
        self.scope.set_command_lookup_mode(command);
        let result = (|| {
            let previous = self.ctes(command.ctes(), None)?;
            let result = self.command_inner(command);
            self.scope.restore_cte_schemas(previous);
            result
        })();
        self.scope.resolution.set_lookup_mode(lookup);
        result
    }

    fn command_inner(&mut self, command: &CommandPlan) -> Result<Option<RowSchema>, SQLError> {
        if command.mutation_target().is_none() {
            for query in command.query_inputs() {
                self.query(query, None)?;
            }
            return Ok(None);
        }
        if let Some(source) = command.source_input() {
            self.source(source, command.scalar_subqueries(), None)?;
        }
        let (target, input) = self.scope.command_expression_schema(
            self.routines,
            command,
            &self.parameters.values(),
        )?;
        let subqueries = command.scalar_subqueries();
        match command {
            CommandPlan::Insert(insert) => {
                validate_target_columns(&insert.table, insert.columns.iter(), &target)?;
                for row in &insert.rows {
                    self.insert_row(row, &insert.columns, &target, &input, subqueries)?;
                }
                if let Some(source) = &insert.source {
                    let mut output = self.query_output(source, None, true)?;
                    self.insert_values(&mut output.types, &insert.columns, &target)?;
                }
                if let Some(conflict) = &insert.on_conflict {
                    for expression in &conflict.expressions {
                        self.expression(expression, &input, subqueries)?;
                    }
                    if let Some(predicate) = &conflict.predicate {
                        self.expression(predicate, &input, subqueries)?;
                    }
                    if let ConflictActionPlan::Update {
                        assignments,
                        predicate,
                    } = &conflict.action
                    {
                        validate_target_columns(
                            &insert.table,
                            assignments.iter().map(|assignment| &assignment.column),
                            &target,
                        )?;
                        let excluded = RowSchema::with_qualified_types(
                            "excluded",
                            target.columns().to_vec(),
                            target.column_types().to_vec(),
                        );
                        let conflict_input =
                            RowSchema::join(&input, &excluded, std::iter::empty::<String>());
                        self.assignments(assignments, &target, &conflict_input, subqueries)?;
                        if let Some(predicate) = predicate {
                            self.require_boolean(predicate, &conflict_input, subqueries, "WHERE")?;
                        }
                    }
                }
            }
            CommandPlan::Update(update) => {
                validate_target_columns(
                    &update.table,
                    update
                        .assignments
                        .iter()
                        .map(|assignment| &assignment.column),
                    &target,
                )?;
                if let Some(predicate) = &update.predicate {
                    self.require_boolean(predicate, &input, subqueries, "WHERE")?;
                }
                self.assignments(&update.assignments, &target, &input, subqueries)?;
            }
            CommandPlan::Delete(delete) => {
                if let Some(predicate) = &delete.predicate {
                    self.require_boolean(predicate, &input, subqueries, "WHERE")?;
                }
            }
            CommandPlan::Merge(merge) => self.merge(merge, &target, &input, subqueries)?,
            _ => {}
        }
        let returning = command.returning().unwrap_or_default();
        if returning.is_empty() {
            return Ok(None);
        }
        let mut output = self.projections(returning, &target, &input, subqueries)?;
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
        self.require_boolean(&merge.join_condition, input, subqueries, "JOIN/ON")?;
        if let Some(predicate) = &merge.target_predicate {
            self.require_boolean(predicate, input, subqueries, "WHERE")?;
        }
        let target_qualifier = merge
            .target_alias
            .as_deref()
            .unwrap_or(&merge.target_qualifier);
        let target_input = RowSchema::with_qualified_types(
            target_qualifier,
            target.columns().to_vec(),
            target.column_types().to_vec(),
        );
        let source_input = self.source(&merge.source, subqueries, None)?;
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
                self.require_boolean(condition, input, subqueries, "WHEN")?;
            }
            match clause {
                MergeWhenPlan::UpdateMatched { assignments, .. }
                | MergeWhenPlan::UpdateNotMatchedBySource { assignments, .. } => {
                    validate_target_columns(
                        &merge.target,
                        assignments.iter().map(|assignment| &assignment.column),
                        target,
                    )?;
                    self.assignments(assignments, target, input, subqueries)?;
                }
                MergeWhenPlan::InsertNotMatched {
                    columns, values, ..
                } => {
                    validate_target_columns(&merge.target, columns.iter(), target)?;
                    self.insert_row(values, columns, target, input, subqueries)?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn assignments(
        &mut self,
        assignments: &[AssignmentPlan],
        target: &RowSchema,
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<(), SQLError> {
        let mut values = assignments
            .iter()
            .map(|assignment| self.expression(&assignment.value, input, subqueries))
            .collect::<Result<Vec<_>, _>>()?;
        for (assignment, value) in assignments.iter().zip(&mut values) {
            self.assignment(value, &assignment.column, target)?;
        }
        Ok(())
    }

    fn insert_row(
        &mut self,
        row: &[ScalarExpr],
        columns: &[String],
        target: &RowSchema,
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<(), SQLError> {
        let mut values = row
            .iter()
            .map(|expression| self.expression(expression, input, subqueries))
            .collect::<Result<Vec<_>, _>>()?;
        self.insert_values(&mut values, columns, target)
    }

    fn insert_values(
        &mut self,
        values: &mut [ExpressionType],
        columns: &[String],
        target: &RowSchema,
    ) -> Result<(), SQLError> {
        let names = if columns.is_empty() {
            target.columns()
        } else {
            columns
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
        for (value, column) in values.iter_mut().zip(names) {
            self.assignment(value, column, target)?;
        }
        Ok(())
    }

    fn assignment(
        &mut self,
        value: &mut ExpressionType,
        column: &str,
        target: &RowSchema,
    ) -> Result<(), SQLError> {
        if target.columns_are_open(None) && target.unqualified_position(column).is_none() {
            return Ok(());
        }
        let index = target
            .columns()
            .iter()
            .position(|name| name == column)
            .ok_or_else(|| error("42703", format!("column \"{column}\" does not exist")))?;
        let Some(ty) = target.column_type(index) else {
            return Ok(());
        };
        self.parameters.coerce_unknown(value, ty)?;
        if let Some(source) = &value.ty {
            if !crate::assignment_type_compatible(source, ty) {
                return Err(error(
                    "42804",
                    format!(
                        "column \"{column}\" is of type {} but expression is of type {}",
                        ty.sql_name(),
                        source.sql_name()
                    ),
                ));
            }
        }
        Ok(())
    }
}

fn validate_target_columns<'a>(
    table: &str,
    columns: impl Iterator<Item = &'a String>,
    target: &RowSchema,
) -> Result<(), SQLError> {
    if target.columns_are_open(None) {
        return Ok(());
    }
    for column in columns {
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
