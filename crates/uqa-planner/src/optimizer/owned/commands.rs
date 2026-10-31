//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Command planning preserves assignment-value, qualification and RETURNING initialization boundaries.

use super::{Context, MutableCommandPlan, SQLError};
use crate::{CommandPlan, ConflictActionPlan, MergeWhenPlan};
use uqa_sql::plan::{CommandAssignmentSlot, CommandQuerySlot, CommandScalarSlot};

impl Context<'_, '_> {
    pub(super) fn command(
        &self,
        mut command: MutableCommandPlan,
    ) -> Result<MutableCommandPlan, SQLError> {
        let control = &self.config.production;
        command = command.prune_subqueries(control)?;
        let reachable =
            uqa_sql::semantics::reachable_command_cte_order_with_control(&command, *control)?;
        for index in 0..command.ctes().len() {
            control.check()?;
            if reachable.contains(&index) {
                command = command.map_cte(index, control, |body| self.plan(body))?;
            }
        }
        drop(reachable);
        match &*command {
            CommandPlan::Insert(_) => self.insert(command),
            CommandPlan::Update(_) => self.update(command),
            CommandPlan::Delete(_) => {
                command = command.map_source(control, |source| self.source(source))?;
                command =
                    command.map_scalar(CommandScalarSlot::Predicate, control, |expression| {
                        self.access_predicate(self.qualification(expression)?)
                    })?;
                self.mutation_tail(command)
            }
            CommandPlan::Merge(_) => self.merge(command),
            CommandPlan::DeclareCursor { .. } => {
                command.map_query_body(control, |query| self.query(query))
            }
            CommandPlan::Call { args, .. } => {
                for argument in 0..args.len() {
                    command = command.map_scalar(
                        CommandScalarSlot::CallArgument(argument),
                        control,
                        |expression| self.scalar(expression),
                    )?;
                    let CommandPlan::Call { args, .. } = &*command else {
                        unreachable!()
                    };
                    let count = args[argument].subqueries.len();
                    let live =
                        super::super::subqueries::live_slots([&args[argument].scalar], *control)?;
                    for (&query, ()) in &live {
                        if query < count {
                            command = command.map_query_child(
                                CommandQuerySlot::CallArgumentSubquery { argument, query },
                                control,
                                |query| self.query(query),
                            )?;
                        }
                    }
                }
                Ok(command)
            }
            // Utility bodies keep their established preparation boundary; the statement-level structural passes have already visited their query owners.
            _ => Ok(command),
        }
    }

    fn insert(&self, mut command: MutableCommandPlan) -> Result<MutableCommandPlan, SQLError> {
        let control = &self.config.production;
        command = command.map_query_child(CommandQuerySlot::InsertSource, control, |query| {
            self.query(query)
        })?;
        let CommandPlan::Insert(plan) = &*command else {
            unreachable!()
        };
        if plan.rows.iter().any(|row| !row.is_empty()) {
            let config = self.config.for_values_without_active()?;
            let values = Context {
                config: &config,
                aggregates: self.aggregates,
                active: self.active,
            };
            for row in 0..plan.rows.len() {
                let CommandPlan::Insert(plan) = &*command else {
                    unreachable!()
                };
                for column in 0..plan.rows[row].len() {
                    command = command.map_scalar(
                        CommandScalarSlot::InsertValue { row, column },
                        control,
                        |expression| values.scalar(expression),
                    )?;
                }
            }
        }
        let CommandPlan::Insert(plan) = &*command else {
            unreachable!()
        };
        if let Some(conflict) = &plan.on_conflict {
            if let ConflictActionPlan::Update { assignments, .. } = &conflict.action {
                for index in 0..assignments.len() {
                    command = command.map_assignment_value(
                        CommandAssignmentSlot::Conflict(index),
                        control,
                        |expression| self.scalar(expression),
                    )?;
                }
                command = command.map_scalar(
                    CommandScalarSlot::ConflictPredicate,
                    control,
                    |expression| self.qualification(expression),
                )?;
            }
        }
        self.mutation_tail(command)
    }

    fn update(&self, mut command: MutableCommandPlan) -> Result<MutableCommandPlan, SQLError> {
        let control = &self.config.production;
        command = command.map_source(control, |source| self.source(source))?;
        let CommandPlan::Update(plan) = &*command else {
            unreachable!()
        };
        for index in 0..plan.assignments.len() {
            command = command.map_assignment_value(
                CommandAssignmentSlot::Update(index),
                control,
                |expression| self.scalar(expression),
            )?;
        }
        command = command.map_scalar(CommandScalarSlot::Predicate, control, |expression| {
            self.access_predicate(self.qualification(expression)?)
        })?;
        self.mutation_tail(command)
    }

    fn merge(&self, mut command: MutableCommandPlan) -> Result<MutableCommandPlan, SQLError> {
        let control = &self.config.production;
        command = command.map_source(control, |source| self.source(source))?;
        command = command.map_scalar(CommandScalarSlot::MergeJoin, control, |expression| {
            self.qualification(expression)
        })?;
        let CommandPlan::Merge(plan) = &*command else {
            unreachable!()
        };
        for clause in 0..plan.when_clauses.len() {
            command = command.map_scalar(
                CommandScalarSlot::MergeCondition(clause),
                control,
                |expression| self.qualification(expression),
            )?;
            let CommandPlan::Merge(plan) = &*command else {
                unreachable!()
            };
            match &plan.when_clauses[clause] {
                MergeWhenPlan::UpdateMatched { assignments, .. }
                | MergeWhenPlan::UpdateNotMatchedBySource { assignments, .. } => {
                    for assignment in 0..assignments.len() {
                        command = command.map_assignment_value(
                            CommandAssignmentSlot::Merge { clause, assignment },
                            control,
                            |expression| self.scalar(expression),
                        )?;
                    }
                }
                MergeWhenPlan::InsertNotMatched { values, .. } => {
                    for value in 0..values.len() {
                        command = command.map_scalar(
                            CommandScalarSlot::MergeValue { clause, value },
                            control,
                            |expression| self.scalar(expression),
                        )?;
                    }
                }
                _ => {}
            }
        }
        self.mutation_tail(command)
    }

    fn mutation_tail(
        &self,
        mut command: MutableCommandPlan,
    ) -> Result<MutableCommandPlan, SQLError> {
        let control = &self.config.production;
        let count = command.returning().expect("mutation RETURNING").len();
        for index in 0..count {
            let projections = command.returning().expect("mutation RETURNING");
            if let Some(alias) = super::projection_alias(&projections[index], control)? {
                command = command.replace_returning_alias(index, Some(alias), control)?;
            }
            command =
                command.map_scalar(CommandScalarSlot::Returning(index), control, |expression| {
                    self.scalar(expression)
                })?;
        }
        let (live, count) = match &*command {
            CommandPlan::Insert(plan) => (
                super::super::subqueries::live_slots(plan.expressions(), *control)?,
                plan.subqueries.len(),
            ),
            CommandPlan::Update(plan) => (
                super::super::subqueries::live_slots_with_source(
                    plan.expressions(),
                    plan.source.as_deref(),
                    *control,
                )?,
                plan.subqueries.len(),
            ),
            CommandPlan::Delete(plan) => (
                super::super::subqueries::live_slots_with_source(
                    plan.expressions(),
                    plan.source.as_deref(),
                    *control,
                )?,
                plan.subqueries.len(),
            ),
            CommandPlan::Merge(plan) => (
                super::super::subqueries::live_slots_with_source(
                    plan.expressions(),
                    Some(&plan.source),
                    *control,
                )?,
                plan.subqueries.len(),
            ),
            _ => unreachable!("mutation tail"),
        };
        for (&index, ()) in &live {
            control.check()?;
            if index < count {
                command = command.map_query_child(
                    CommandQuerySlot::Subquery(index),
                    control,
                    |query| self.query(query),
                )?;
            }
        }
        Ok(command)
    }
}
