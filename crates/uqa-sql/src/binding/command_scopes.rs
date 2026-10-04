//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The relations each clause of a data-modifying statement can name, as `PostgreSQL`'s parser builds the namespace of the clause: the rows and the source query of an `INSERT` see no relation, its `ON CONFLICT` and `RETURNING` lists see the target, and `DO UPDATE` also sees `excluded`; the clauses of `UPDATE` and `DELETE` see the target with their other sources, which themselves do not see the target; and each `MERGE` action sees the relations its match kind provides.

use super::{
    overlay_outer_schema, QueryPlan, RowSchema, SQLError, SQLParam, ScalarExpr, SchemaScope,
};
use crate::ast::AssignmentTarget;
use crate::plan::{
    AssignmentPlan, CommandPlan, ConflictActionPlan, DeletePlan, InsertPlan, MergePlan,
    MergeWhenPlan, UpdatePlan,
};
use crate::routines::RoutineResolution;
use std::collections::BTreeSet;

/// The scalar expressions of one clause and the row scope they resolve against.
struct ClauseScope<'a> {
    schema: RowSchema,
    expressions: Vec<&'a mut ScalarExpr>,
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
        let previous = match command.ctes_mut() {
            Some(ctes) => self.bind_cte_routine_schemas(routines, ctes, params, outer)?,
            None => Vec::new(),
        };
        let result = self.bind_command_clauses_for_storage(routines, command, params, outer);
        self.restore_cte_schemas(previous);
        result
    }

    fn bind_command_clauses_for_storage(
        &mut self,
        routines: &dyn RoutineResolution,
        command: &mut CommandPlan,
        params: &[SQLParam],
        outer: Option<&RowSchema>,
    ) -> Result<(), SQLError> {
        let subqueries = command.scalar_subqueries().to_vec();
        if let Some(source) = command.source_input_mut() {
            self.bind_source_routines_for_storage(routines, source, &subqueries, params, outer)?;
        }
        let (target, expression) = self.command_expression_schema(routines, command, params)?;
        match command {
            CommandPlan::Insert(insert) => {
                self.bind_insert_clauses(routines, insert, &target, &expression, params, outer)
            }
            CommandPlan::Update(update) => {
                let UpdatePlan {
                    assignments,
                    predicate,
                    returning,
                    subqueries,
                    ..
                } = update.as_mut();
                let schema = overlay_outer_schema(&expression, outer);
                let expressions = assignments
                    .iter_mut()
                    .flat_map(AssignmentPlan::expressions_mut)
                    .chain(predicate.as_mut())
                    .chain(returning.iter_mut().map(|projection| &mut projection.expr))
                    .collect();
                let clauses = vec![ClauseScope {
                    schema: schema.clone(),
                    expressions,
                }];
                self.bind_clause_scopes(routines, clauses, subqueries, &schema, params, outer)
            }
            CommandPlan::Delete(delete) => {
                let DeletePlan {
                    predicate,
                    returning,
                    subqueries,
                    ..
                } = delete.as_mut();
                let schema = overlay_outer_schema(&expression, outer);
                let expressions = predicate
                    .as_mut()
                    .into_iter()
                    .chain(returning.iter_mut().map(|projection| &mut projection.expr))
                    .collect();
                let clauses = vec![ClauseScope {
                    schema: schema.clone(),
                    expressions,
                }];
                self.bind_clause_scopes(routines, clauses, subqueries, &schema, params, outer)
            }
            CommandPlan::Merge(merge) => {
                self.bind_merge_clauses(routines, merge, &target, &expression, params, outer)
            }
            _ => Ok(()),
        }
    }

    fn bind_insert_clauses(
        &mut self,
        routines: &dyn RoutineResolution,
        insert: &mut InsertPlan,
        target: &RowSchema,
        expression: &RowSchema,
        params: &[SQLParam],
        outer: Option<&RowSchema>,
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
        if let Some(source) = source {
            self.bind_query_routines_for_storage(routines, source, params, outer)?;
        }
        let excluded = RowSchema::with_qualified_types(
            "excluded",
            target.columns().to_vec(),
            target.column_types().to_vec(),
        );
        let conflict_update = overlay_outer_schema(
            &RowSchema::join(expression, &excluded, std::iter::empty::<String>()),
            outer,
        );
        let expression = overlay_outer_schema(expression, outer);
        let mut arbiter = Vec::new();
        let mut update = Vec::new();
        if let Some(conflict) = on_conflict {
            arbiter.extend(conflict.expressions.iter_mut());
            arbiter.extend(conflict.predicate.as_deref_mut());
            if let ConflictActionPlan::Update {
                assignments,
                predicate,
            } = &mut conflict.action
            {
                update.extend(
                    assignments
                        .iter_mut()
                        .flat_map(AssignmentPlan::expressions_mut),
                );
                update.extend(predicate.as_deref_mut());
            }
        }
        let clauses = vec![
            // The target is in the range table but not in the namespace of the rows.
            ClauseScope {
                schema: outer.cloned().unwrap_or_default(),
                expressions: columns
                    .iter_mut()
                    .flat_map(AssignmentTarget::expressions_mut)
                    .chain(rows.iter_mut().flatten())
                    .collect(),
            },
            ClauseScope {
                schema: expression.clone(),
                expressions: arbiter,
            },
            ClauseScope {
                schema: conflict_update,
                expressions: update,
            },
            ClauseScope {
                schema: expression.clone(),
                expressions: returning
                    .iter_mut()
                    .map(|projection| &mut projection.expr)
                    .collect(),
            },
        ];
        self.bind_clause_scopes(routines, clauses, subqueries, &expression, params, outer)
    }

    fn bind_merge_clauses(
        &mut self,
        routines: &dyn RoutineResolution,
        merge: &mut MergePlan,
        target: &RowSchema,
        joined: &RowSchema,
        params: &[SQLParam],
        outer: Option<&RowSchema>,
    ) -> Result<(), SQLError> {
        let qualifier = merge
            .target_alias
            .as_deref()
            .unwrap_or(&merge.target_qualifier);
        let target = overlay_outer_schema(
            &RowSchema::with_qualified_types(
                qualifier,
                target.columns().to_vec(),
                target.column_types().to_vec(),
            ),
            outer,
        );
        let source = overlay_outer_schema(
            &self.bind_source(routines, &merge.source, &merge.subqueries, params, None)?,
            outer,
        );
        let joined = overlay_outer_schema(joined, outer);
        let MergePlan {
            join_condition,
            target_predicate,
            when_clauses,
            returning,
            subqueries,
            ..
        } = merge;
        let mut matched = vec![join_condition];
        matched.extend(target_predicate.as_mut());
        let mut not_matched = Vec::new();
        let mut not_matched_by_source = Vec::new();
        for clause in when_clauses {
            match clause {
                MergeWhenPlan::UpdateMatched {
                    condition,
                    assignments,
                } => {
                    matched.extend(condition.as_mut());
                    matched.extend(
                        assignments
                            .iter_mut()
                            .flat_map(AssignmentPlan::expressions_mut),
                    );
                }
                MergeWhenPlan::DeleteMatched { condition }
                | MergeWhenPlan::NothingMatched { condition } => {
                    matched.extend(condition.as_mut());
                }
                MergeWhenPlan::InsertNotMatched {
                    condition,
                    columns,
                    values,
                    ..
                } => {
                    not_matched.extend(condition.as_mut());
                    not_matched.extend(
                        columns
                            .iter_mut()
                            .flat_map(AssignmentTarget::expressions_mut),
                    );
                    not_matched.extend(values.iter_mut());
                }
                MergeWhenPlan::NothingNotMatched { condition } => {
                    not_matched.extend(condition.as_mut());
                }
                MergeWhenPlan::UpdateNotMatchedBySource {
                    condition,
                    assignments,
                } => {
                    not_matched_by_source.extend(condition.as_mut());
                    not_matched_by_source.extend(
                        assignments
                            .iter_mut()
                            .flat_map(AssignmentPlan::expressions_mut),
                    );
                }
                MergeWhenPlan::DeleteNotMatchedBySource { condition }
                | MergeWhenPlan::NothingNotMatchedBySource { condition } => {
                    not_matched_by_source.extend(condition.as_mut());
                }
            }
        }
        matched.extend(returning.iter_mut().map(|projection| &mut projection.expr));
        let clauses = vec![
            ClauseScope {
                schema: joined.clone(),
                expressions: matched,
            },
            ClauseScope {
                schema: source,
                expressions: not_matched,
            },
            ClauseScope {
                schema: target,
                expressions: not_matched_by_source,
            },
        ];
        self.bind_clause_scopes(routines, clauses, subqueries, &joined, params, outer)
    }

    /// Bind each clause's subqueries within the clause's scope, and then the clause's expressions. A subquery that no clause expression names, such as one in a join condition of the statement's sources, is bound within `fallback`.
    fn bind_clause_scopes(
        &mut self,
        routines: &dyn RoutineResolution,
        clauses: Vec<ClauseScope<'_>>,
        subqueries: &mut [QueryPlan],
        fallback: &RowSchema,
        params: &[SQLParam],
        outer: Option<&RowSchema>,
    ) -> Result<(), SQLError> {
        let mut owners = vec![None; subqueries.len()];
        for (position, clause) in clauses.iter().enumerate() {
            let mut referenced = BTreeSet::new();
            for expression in &clause.expressions {
                crate::semantics::collect_subquery_ids(expression, &mut referenced);
            }
            for subquery in referenced {
                if let Some(owner) = owners.get_mut(subquery) {
                    owner.get_or_insert(position);
                }
            }
        }
        for (subquery, owner) in subqueries.iter_mut().zip(&owners) {
            let scope = owner.map_or(fallback, |position| &clauses[position].schema);
            self.bind_query_routines_for_storage(routines, subquery, params, Some(scope))?;
        }
        let subqueries = subqueries.to_vec();
        for clause in clauses {
            for expression in clause.expressions {
                self.bind_scalar_routines_for_storage(
                    routines,
                    expression,
                    &clause.schema,
                    &subqueries,
                    params,
                    outer,
                )?;
            }
        }
        Ok(())
    }
}
