//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Copying, QueryArena, Result};
use crate::{
    ast::{DeleteStmt, InsertStmt, MergeStmt, UpdateStmt},
    plan::{AggregateClassifier, DeletePlan, InsertPlan, MergePlan, UpdatePlan},
};

mod assignments;

impl Copying<'_> {
    pub(super) fn lower_insert(
        &mut self,
        statement: &InsertStmt,
        aggregates: &dyn AggregateClassifier,
    ) -> Result<InsertPlan> {
        let ctes = self.field("ctes", |copying| {
            copying.map(statement.with.iter(), |copying, cte| {
                copying.lower_cte(cte, aggregates)
            })
        })?;
        let source = self.field("source", |copying| {
            statement
                .select_source
                .as_ref()
                .map(|query| copying.boxed(|copying| copying.lower_nested_query(query, aggregates)))
                .transpose()
        })?;
        let mut arena = QueryArena::new(self.control, self.parameters);
        let rows = self.field("rows", |copying| {
            copying.lower_rows(&statement.rows, aggregates, &mut arena)
        })?;
        let on_conflict = self.field("on_conflict", |copying| {
            statement
                .on_conflict
                .as_ref()
                .map(|conflict| copying.lower_conflict(conflict, aggregates, &mut arena))
                .transpose()
        })?;
        let returning = self.field("returning", |copying| {
            copying.map(statement.returning.iter(), |copying, projection| {
                copying.lower_projection(projection, aggregates, &mut arena)
            })
        })?;
        Ok(InsertPlan {
            table: self.text(&statement.table)?,
            target_relation_bound: statement.target_relation_bound,
            relations_bound: false,
            statement_privilege_subject: None,
            target_privilege_subject: None,
            target_qualifier: self.text(&statement.target_qualifier)?,
            target_alias: self.optional_text(statement.target_alias.as_deref())?,
            include_descendants: statement.include_descendants,
            columns: self.field("columns", |copying| {
                copying.map(statement.columns.iter(), |copying, target| {
                    copying.lower_assignment_target(target, aggregates, &mut arena)
                })
            })?,
            overriding: statement.overriding,
            ctes,
            rows,
            source,
            rule_values_input: self.field("rule_values_input", |_| Ok(None))?,
            on_conflict,
            returning,
            returning_aliases: self.returning_aliases(&statement.returning_aliases)?,
            subqueries: self.take_arena(arena)?,
            view_checks: self.field("view_checks", |_| Ok(Vec::new()))?,
            view_rule_relations: Vec::new(),
            view_rule_insert_plans: Vec::new(),
            view_rule_returning: None,
        })
    }

    pub(super) fn lower_update(
        &mut self,
        statement: &UpdateStmt,
        aggregates: &dyn AggregateClassifier,
    ) -> Result<UpdatePlan> {
        let ctes = self.field("ctes", |copying| {
            copying.map(statement.with.iter(), |copying, cte| {
                copying.lower_cte(cte, aggregates)
            })
        })?;
        let mut arena = QueryArena::new(self.control, self.parameters);
        let source = self.field("source", |copying| {
            statement
                .from
                .as_ref()
                .map(|source| {
                    copying.boxed(|copying| copying.lower_source(source, aggregates, &mut arena))
                })
                .transpose()
        })?;
        let assignments = self.field("assignments", |copying| {
            copying.map(statement.assignments.iter(), |copying, assignment| {
                copying.lower_assignment(assignment, aggregates, &mut arena)
            })
        })?;
        let predicate = self.field("predicate", |copying| {
            copying.lower_optional_scalar(statement.r#where.as_ref(), aggregates, &mut arena)
        })?;
        let returning = self.field("returning", |copying| {
            copying.map(statement.returning.iter(), |copying, projection| {
                copying.lower_projection(projection, aggregates, &mut arena)
            })
        })?;
        Ok(UpdatePlan {
            table: self.text(&statement.table)?,
            target_relation_bound: statement.target_relation_bound,
            relations_bound: false,
            statement_privilege_subject: None,
            target_privilege_subject: None,
            target_qualifier: self.text(&statement.target_qualifier)?,
            target_alias: self.optional_text(statement.target_alias.as_deref())?,
            include_descendants: statement.include_descendants,
            assignments,
            predicate,
            ctes,
            source,
            returning,
            returning_aliases: self.returning_aliases(&statement.returning_aliases)?,
            subqueries: self.take_arena(arena)?,
            view_checks: self.field("view_checks", |_| Ok(Vec::new()))?,
            view_rule_relations: Vec::new(),
            view_rule_update_plans: Vec::new(),
            view_rule_returning: None,
        })
    }

    pub(super) fn lower_delete(
        &mut self,
        statement: &DeleteStmt,
        aggregates: &dyn AggregateClassifier,
    ) -> Result<DeletePlan> {
        let ctes = self.field("ctes", |copying| {
            copying.map(statement.with.iter(), |copying, cte| {
                copying.lower_cte(cte, aggregates)
            })
        })?;
        let mut arena = QueryArena::new(self.control, self.parameters);
        let source = self.field("source", |copying| {
            statement
                .using
                .as_ref()
                .map(|source| {
                    copying.boxed(|copying| copying.lower_source(source, aggregates, &mut arena))
                })
                .transpose()
        })?;
        let predicate = self.field("predicate", |copying| {
            copying.lower_optional_scalar(statement.r#where.as_ref(), aggregates, &mut arena)
        })?;
        let returning = self.field("returning", |copying| {
            copying.map(statement.returning.iter(), |copying, projection| {
                copying.lower_projection(projection, aggregates, &mut arena)
            })
        })?;
        Ok(DeletePlan {
            table: self.text(&statement.table)?,
            target_relation_bound: statement.target_relation_bound,
            relations_bound: false,
            statement_privilege_subject: None,
            target_privilege_subject: None,
            target_qualifier: self.text(&statement.target_qualifier)?,
            target_alias: self.optional_text(statement.target_alias.as_deref())?,
            include_descendants: statement.include_descendants,
            predicate,
            ctes,
            source,
            returning,
            returning_aliases: self.returning_aliases(&statement.returning_aliases)?,
            subqueries: self.take_arena(arena)?,
            view_rule_relations: Vec::new(),
            view_rule_returning: None,
        })
    }

    pub(super) fn lower_merge(
        &mut self,
        statement: &MergeStmt,
        aggregates: &dyn AggregateClassifier,
    ) -> Result<MergePlan> {
        let mut arena = QueryArena::new(self.control, self.parameters);
        let source = self.field("source", |copying| {
            copying.boxed(|copying| copying.lower_source(&statement.source, aggregates, &mut arena))
        })?;
        let join_condition = self.field("join_condition", |copying| {
            copying.lower_scalar(&statement.join_condition, aggregates, &mut arena)
        })?;
        let when_clauses = self.field("when_clauses", |copying| {
            copying.map(statement.when_clauses.iter(), |copying, clause| {
                copying.lower_merge_when(clause, aggregates, &mut arena)
            })
        })?;
        let returning = self.field("returning", |copying| {
            copying.map(statement.returning.iter(), |copying, projection| {
                copying.lower_projection(projection, aggregates, &mut arena)
            })
        })?;
        Ok(MergePlan {
            ctes: self.field("ctes", |copying| {
                copying.map(statement.with.iter(), |copying, cte| {
                    copying.lower_cte(cte, aggregates)
                })
            })?,
            target: self.text(&statement.target)?,
            statement_privilege_subject: None,
            target_privilege_subject: None,
            target_qualifier: self.text(&statement.target_qualifier)?,
            target_alias: self.optional_text(statement.target_alias.as_deref())?,
            include_descendants: statement.include_descendants,
            target_predicate: self.field("target_predicate", |_| Ok(None))?,
            source,
            join_condition,
            when_clauses,
            returning,
            returning_aliases: self.returning_aliases(&statement.returning_aliases)?,
            subqueries: self.take_arena(arena)?,
            view_checks: self.field("view_checks", |_| Ok(Vec::new()))?,
        })
    }
}
