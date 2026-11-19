//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The relations each clause of a data-modifying statement can name, as `PostgreSQL`'s parser builds the namespace of the clause: the rows and the source query of an `INSERT` see no relation, its `ON CONFLICT` and `RETURNING` lists see the target, and `DO UPDATE` also sees `excluded`; the clauses of `UPDATE` and `DELETE` see the target with their other sources, which themselves do not see the target; and each `MERGE` action sees the relations its match kind provides.

use super::{
    overlay_outer_schema, QueryPlan, RowSchema, SQLError, SQLParam, ScalarExpr, SchemaScope,
    SourceScope,
};
use crate::ast::AssignmentTarget;
use crate::catalog::work::Work;
use crate::plan::{
    AssignmentPlan, CommandPlan, ConflictActionPlan, DeletePlan, InsertPlan, MergePlan, UpdatePlan,
};
use crate::routines::RoutineResolution;
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

/// The scalar expressions of one clause and the row scope they resolve against.
struct ClauseScope<'a> {
    schema: RowSchema,
    expressions: Produced<Vec<&'a mut ScalarExpr>>,
}

impl<'a> ClauseScope<'a> {
    async fn from_expressions(
        schema: RowSchema,
        values: impl Iterator<Item = &'a mut ScalarExpr> + Send,
        control: &ProductionControl<'_>,
        work: &mut (impl Work + Send),
    ) -> Result<Self, SQLError> {
        let mut expressions = ProductionVec::new(*control);
        Self::append(&mut expressions, values, control, work).await?;
        Ok(Self {
            schema,
            expressions: expressions.finish()?,
        })
    }

    async fn append(
        expressions: &mut ProductionVec<'_, &'a mut ScalarExpr>,
        values: impl Iterator<Item = &'a mut ScalarExpr> + Send,
        control: &ProductionControl<'_>,
        work: &mut (impl Work + Send),
    ) -> Result<(), SQLError> {
        for value in values {
            work.checkpoint().await?;
            expressions.push_produced(control.finish(value, control.empty_reservation())?)?;
        }
        Ok(())
    }
}

/// The row scopes of a data-modifying statement, each over the scope that encloses the statement.
struct CommandScopes {
    /// The target's columns, unqualified.
    target: RowSchema,
    /// The target with the statement's other sources, which every clause but `RETURNING` sees.
    clauses: RowSchema,
    /// What `RETURNING` sees: the clauses' scope with the target's `old` and `new` aliases.
    returning: RowSchema,
}

impl SchemaScope {
    /// Bind the routine calls and resolve the routine parameters of a data-modifying statement, each clause against the relations it can name. `outer` is the scope that encloses the statement.
    pub(super) fn bind_command_routines_for_storage(
        &mut self,
        routines: &dyn RoutineResolution,
        command: &mut CommandPlan,
        params: &[SQLParam],
        outer: Option<&RowSchema>,
    ) -> Result<(), SQLError> {
        crate::catalog::work::inline(self.bind_command_routines_for_storage_with_work(
            routines,
            command,
            params,
            outer,
            &mut (),
        ))
    }

    pub(super) async fn bind_command_routines_for_storage_with_work<W: Work + Send>(
        &mut self,
        routines: &dyn RoutineResolution,
        command: &mut CommandPlan,
        params: &[SQLParam],
        outer: Option<&RowSchema>,
        work: &mut W,
    ) -> Result<(), SQLError> {
        let previous = match command.ctes_mut() {
            Some(ctes) => {
                self.bind_cte_routine_schemas(routines, ctes, params, outer, work)
                    .await?
            }
            None => super::ctes::CteScopeStack::new(0, &self.control())?,
        };
        let result = self
            .bind_command_clauses_for_storage(routines, command, params, outer, work)
            .await;
        self.restore_cte_schemas(previous);
        result
    }

    async fn bind_command_clauses_for_storage<W: Work + Send>(
        &mut self,
        routines: &dyn RoutineResolution,
        command: &mut CommandPlan,
        params: &[SQLParam],
        outer: Option<&RowSchema>,
        work: &mut W,
    ) -> Result<(), SQLError> {
        let preceding = if command.source_input().is_some() {
            let target = self.bind_command_target(routines, command, params)?;
            SourceScope::command_target(command, &target)
        } else {
            None
        };
        if let Some((source, subqueries)) = command.source_and_subqueries_mut() {
            self.bind_source_routines_in_scope_with_work(
                routines,
                source,
                subqueries,
                params,
                SourceScope {
                    outer,
                    preceding: preceding.as_ref(),
                    inherited_lateral: false,
                    retain_names: true,
                },
                work,
            )
            .await?;
        }
        let scopes = self
            .command_scopes(routines, command, params, outer, work)
            .await?;
        let labels = command
            .returning()
            .map(super::routine_parameters::column_labels)
            .unwrap_or_default();
        self.bind_command_clause_scopes(routines, command, &scopes, params, outer, work)
            .await?;
        if let Some(returning) = command.returning_mut() {
            super::routine_parameters::keep_column_labels(returning, labels);
        }
        Ok(())
    }

    async fn command_scopes<W: Work + Send>(
        &mut self,
        routines: &dyn RoutineResolution,
        command: &CommandPlan,
        params: &[SQLParam],
        outer: Option<&RowSchema>,
        work: &mut W,
    ) -> Result<CommandScopes, SQLError> {
        let (target, returning, _) =
            self.command_expression_schema(routines, command, params, outer)?;
        let source = match command.source_input() {
            Some(source) => Some(
                self.bind_source_in_scope_with_work(
                    routines,
                    source,
                    command.scalar_subqueries(),
                    params,
                    SourceScope::new(outer),
                    work,
                )
                .await?,
            ),
            None => None,
        };
        let qualifier = command.target_qualifier().unwrap_or_default();
        let mut clauses =
            crate::semantics::mutation_clause_schema(&target, qualifier, source.as_ref());
        if target.columns_are_open(None) {
            clauses = RowSchema::with_open_columns(&clauses, command.target_qualifier());
        }
        Ok(CommandScopes {
            clauses: overlay_outer_schema(&clauses, outer),
            returning: overlay_outer_schema(&returning, outer),
            target,
        })
    }

    async fn bind_command_clause_scopes<W: Work + Send>(
        &mut self,
        routines: &dyn RoutineResolution,
        command: &mut CommandPlan,
        scopes: &CommandScopes,
        params: &[SQLParam],
        outer: Option<&RowSchema>,
        work: &mut W,
    ) -> Result<(), SQLError> {
        let owner = self.production.clone();
        let control = owner.control();
        match command {
            CommandPlan::Insert(insert) => {
                self.bind_insert_clauses(routines, insert, scopes, params, outer, work)
                    .await
            }
            CommandPlan::Update(update) => {
                let UpdatePlan {
                    assignments,
                    predicate,
                    returning,
                    subqueries,
                    ..
                } = update.as_mut();
                let clauses = [
                    ClauseScope::from_expressions(
                        scopes.clauses.clone(),
                        assignments
                            .iter_mut()
                            .flat_map(AssignmentPlan::expressions_mut)
                            .chain(predicate.as_mut()),
                        &control,
                        work,
                    )
                    .await?,
                    ClauseScope::from_expressions(
                        scopes.returning.clone(),
                        returning.iter_mut().map(|projection| &mut projection.expr),
                        &control,
                        work,
                    )
                    .await?,
                ];
                self.bind_clause_scopes(
                    routines,
                    clauses,
                    subqueries,
                    &scopes.clauses,
                    params,
                    work,
                )
                .await
            }
            CommandPlan::Delete(delete) => {
                let DeletePlan {
                    predicate,
                    returning,
                    subqueries,
                    ..
                } = delete.as_mut();
                let clauses = [
                    ClauseScope::from_expressions(
                        scopes.clauses.clone(),
                        predicate.iter_mut(),
                        &control,
                        work,
                    )
                    .await?,
                    ClauseScope::from_expressions(
                        scopes.returning.clone(),
                        returning.iter_mut().map(|projection| &mut projection.expr),
                        &control,
                        work,
                    )
                    .await?,
                ];
                self.bind_clause_scopes(
                    routines,
                    clauses,
                    subqueries,
                    &scopes.clauses,
                    params,
                    work,
                )
                .await
            }
            CommandPlan::Merge(merge) => {
                self.bind_merge_clauses(routines, merge, scopes, params, outer, work)
                    .await
            }
            _ => Ok(()),
        }
    }

    async fn bind_insert_clauses<W: Work + Send>(
        &mut self,
        routines: &dyn RoutineResolution,
        insert: &mut InsertPlan,
        scopes: &CommandScopes,
        params: &[SQLParam],
        outer: Option<&RowSchema>,
        work: &mut W,
    ) -> Result<(), SQLError> {
        let InsertPlan {
            columns,
            rows,
            source,
            on_conflict,
            returning,
            subqueries,
            ..
        } = insert;
        let owner = self.production.clone();
        let control = owner.control();
        if let Some(source) = source {
            crate::catalog::work::nested!(
                W::MAY_SUSPEND,
                control,
                self.bind_query_routines_for_storage_with_work(
                    routines, source, params, outer, work
                )
            )?;
        }
        let excluded = RowSchema::with_relation_qualifier(&scopes.target, "excluded");
        let conflict_update = overlay_outer_schema(
            &RowSchema::join(&scopes.clauses, &excluded, std::iter::empty::<String>()),
            outer,
        );
        let mut arbiter = ProductionVec::new(control);
        let mut update = ProductionVec::new(control);
        if let Some(conflict) = on_conflict {
            ClauseScope::append(
                &mut arbiter,
                conflict
                    .expressions
                    .iter_mut()
                    .chain(conflict.predicate.as_deref_mut()),
                &control,
                work,
            )
            .await?;
            if let ConflictActionPlan::Update {
                assignments,
                predicate,
            } = &mut conflict.action
            {
                ClauseScope::append(
                    &mut update,
                    assignments
                        .iter_mut()
                        .flat_map(AssignmentPlan::expressions_mut)
                        .chain(predicate.as_deref_mut()),
                    &control,
                    work,
                )
                .await?;
            }
        }
        let clauses = [
            ClauseScope::from_expressions(
                outer.cloned().unwrap_or_default(),
                columns
                    .iter_mut()
                    .flat_map(AssignmentTarget::expressions_mut)
                    .chain(rows.iter_mut().flatten()),
                &control,
                work,
            )
            .await?,
            ClauseScope {
                schema: scopes.clauses.clone(),
                expressions: arbiter.finish()?,
            },
            ClauseScope {
                schema: conflict_update,
                expressions: update.finish()?,
            },
            ClauseScope::from_expressions(
                scopes.returning.clone(),
                returning.iter_mut().map(|projection| &mut projection.expr),
                &control,
                work,
            )
            .await?,
        ];
        self.bind_clause_scopes(routines, clauses, subqueries, &scopes.clauses, params, work)
            .await
    }

    async fn bind_merge_clauses<W: Work + Send>(
        &mut self,
        routines: &dyn RoutineResolution,
        merge: &mut MergePlan,
        scopes: &CommandScopes,
        params: &[SQLParam],
        outer: Option<&RowSchema>,
        work: &mut W,
    ) -> Result<(), SQLError> {
        let qualifier = merge
            .target_alias
            .as_deref()
            .unwrap_or(&merge.target_qualifier);
        let target = overlay_outer_schema(
            &RowSchema::with_relation_qualifier(&scopes.target, qualifier),
            outer,
        );
        let source = overlay_outer_schema(
            &self
                .bind_source_in_scope_with_work(
                    routines,
                    &merge.source,
                    &merge.subqueries,
                    params,
                    SourceScope::new(None),
                    work,
                )
                .await?,
            outer,
        );
        let MergePlan {
            join_condition,
            target_predicate,
            when_clauses,
            returning,
            subqueries,
            ..
        } = merge;
        let owner = self.production.clone();
        let control = owner.control();
        let mut matched = ProductionVec::new(control);
        let mut missing_target = ProductionVec::new(control);
        let mut missing_source = ProductionVec::new(control);
        matched.push_produced(control.finish(join_condition, control.empty_reservation())?)?;
        for clause in when_clauses {
            work.checkpoint().await?;
            let selected =
                clause.input_scope(&mut matched, &mut missing_source, &mut missing_target);
            ClauseScope::append(selected, clause.expressions_mut(), &control, work).await?;
        }
        ClauseScope::append(&mut matched, target_predicate.iter_mut(), &control, work).await?;
        let clauses = [
            ClauseScope {
                schema: scopes.clauses.clone(),
                expressions: matched.finish()?,
            },
            ClauseScope {
                schema: source,
                expressions: missing_target.finish()?,
            },
            ClauseScope {
                schema: target,
                expressions: missing_source.finish()?,
            },
            ClauseScope::from_expressions(
                scopes.returning.clone(),
                returning.iter_mut().map(|projection| &mut projection.expr),
                &control,
                work,
            )
            .await?,
        ];
        self.bind_clause_scopes(routines, clauses, subqueries, &scopes.clauses, params, work)
            .await
    }

    /// Bind each clause's subqueries within the clause's scope, and then the clause's expressions. A subquery that no clause expression names, such as one in a join condition of the statement's sources, is bound within `fallback`.
    async fn bind_clause_scopes<W: Work + Send, const N: usize>(
        &mut self,
        routines: &dyn RoutineResolution,
        clauses: [ClauseScope<'_>; N],
        subqueries: &mut [QueryPlan],
        fallback: &RowSchema,
        params: &[SQLParam],
        work: &mut W,
    ) -> Result<(), SQLError> {
        let owner = self.production.clone();
        let control = owner.control();
        if !subqueries.is_empty() {
            let mut owners = ProductionVec::new(control);
            for _ in 0..subqueries.len() {
                work.checkpoint().await?;
                owners.push_copy(None)?;
            }
            for (slot, clause) in clauses.iter().enumerate() {
                for expression in clause.expressions.iter() {
                    crate::ir::observation::visit_in(
                        expression,
                        &control,
                        work,
                        crate::ir::children::Scope::Complete,
                        &mut |node| {
                            if let Some(owner) = crate::semantics::subquery_reference(node)
                                .and_then(|id| owners.as_mut_slice().get_mut(id))
                            {
                                owner.get_or_insert(slot);
                            }
                            Ok(crate::ir::observation::Decision::Descend)
                        },
                    )
                    .await?;
                }
            }
            for (subquery, selected) in subqueries.iter_mut().zip(owners.iter()) {
                let schema = selected.map_or(fallback, |slot| &clauses[slot].schema);
                crate::catalog::work::nested!(
                    W::MAY_SUSPEND,
                    control,
                    self.bind_query_routines_for_storage_with_work(
                        routines,
                        subquery,
                        params,
                        Some(schema),
                        work
                    )
                )?;
            }
        }
        for clause in clauses {
            let (expressions, _memory) = clause.expressions.into_parts();
            for expression in expressions {
                self.bind_scalar_routines_for_storage_with_work(
                    routines,
                    expression,
                    &clause.schema,
                    subqueries,
                    params,
                    work,
                )
                .await?;
            }
        }
        Ok(())
    }
}
