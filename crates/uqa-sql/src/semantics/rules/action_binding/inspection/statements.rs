//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered action-body and local CTE observation shares the rule binder's scopes.

use super::{collect_visible_scope, Inspector, RuleBindingContext, RuleBindingScope};
use crate::{
    ast::{AssignmentTargets, CteBody, Expr, InsertStmt, OnConflictAction, Statement, CTE},
    SQLError,
};

impl Inspector<'_> {
    pub(super) fn statement(
        &mut self,
        input: &Statement,
        inherited: &RuleBindingScope,
        context: &RuleBindingContext<'_>,
    ) -> Result<(), SQLError> {
        match input {
            Statement::Select(query) => self.select(query, inherited, context),
            Statement::Insert(insert) => self.insert(insert, inherited, context),
            Statement::Update(update) => {
                self.ctes(&update.with, inherited, context)?;
                let context = context.with_ctes(&update.with)?;
                let mut scope = inherited.copy(&context.control())?;
                scope.insert_qualifier(&update.target_qualifier, &context.control())?;
                if let Some(source) = &update.from {
                    self.source(source, &scope, &context)?;
                    collect_visible_scope(source, &context, &mut scope)?;
                }
                self.assignments(&update.assignments, &scope, &context)?;
                self.optional(update.r#where.as_ref(), &scope, &context)
            }
            Statement::Delete(delete) => {
                self.ctes(&delete.with, inherited, context)?;
                let context = context.with_ctes(&delete.with)?;
                let mut scope = inherited.copy(&context.control())?;
                scope.insert_qualifier(&delete.target_qualifier, &context.control())?;
                if let Some(source) = &delete.using {
                    self.source(source, &scope, &context)?;
                    collect_visible_scope(source, &context, &mut scope)?;
                }
                self.optional(delete.r#where.as_ref(), &scope, &context)
            }
            // Non-rule utility syntax retains the procedural binder's own namespace and order.
            _ => crate::plpgsql::bind_statement(input, self.resolver).map(drop),
        }
    }

    fn insert(
        &mut self,
        insert: &InsertStmt,
        inherited: &RuleBindingScope,
        context: &RuleBindingContext<'_>,
    ) -> Result<(), SQLError> {
        self.ctes(&insert.with, inherited, context)?;
        let context = context.with_ctes(&insert.with)?;
        for target in &insert.columns {
            for expression in target.expressions() {
                self.expression(expression, inherited, &context)?;
            }
        }
        for row in &insert.rows {
            for expression in row {
                self.expanding(expression, inherited, &context)?;
            }
        }
        if let Some(query) = &insert.select_source {
            self.select(query, inherited, &context)?;
        }
        if let Some(conflict) = &insert.on_conflict {
            let mut scope = inherited.copy(&context.control())?;
            scope.insert_qualifier(&insert.target_qualifier, &context.control())?;
            self.optional(conflict.predicate.as_deref(), &scope, &context)?;
            self.expressions(&conflict.expressions, &scope, &context)?;
            if let OnConflictAction::Update {
                assignments,
                r#where,
            } = &conflict.action
            {
                self.assignments(assignments, &scope, &context)?;
                self.optional(r#where.as_deref(), &scope, &context)?;
            }
        }
        Ok(())
    }

    fn assignments(
        &mut self,
        assignments: &[(AssignmentTargets, Expr)],
        scope: &RuleBindingScope,
        context: &RuleBindingContext<'_>,
    ) -> Result<(), SQLError> {
        for (targets, expression) in assignments {
            for target in targets.expressions() {
                self.expression(target, scope, context)?;
            }
            self.expression(expression, scope, context)?;
        }
        Ok(())
    }

    pub(super) fn ctes(
        &mut self,
        ctes: &[CTE],
        inherited: &RuleBindingScope,
        context: &RuleBindingContext<'_>,
    ) -> Result<(), SQLError> {
        let mut visible = context.copy()?;
        for cte in ctes {
            visible.prepare_recursive_cte(cte)?;
            if let Some(cycle) = &cte.cycle {
                self.expression(&cycle.mark_value, inherited, &visible)?;
                self.expression(&cycle.mark_default, inherited, &visible)?;
            }
            // Command CTE row images are local to that command, just as in action binding.
            if let CteBody::Query(query) = &cte.body {
                self.select(query, inherited, &visible)?;
            }
            visible.record_cte(cte)?;
        }
        Ok(())
    }
}
