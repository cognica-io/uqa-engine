//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed variable sites follow procedural binding order without constructing a second syntax tree.

use crate::{
    ast::{
        AssignmentTarget, AssignmentTargets, CteBody, DeleteStmt, Expr, FrameBound, FromClause,
        InsertStmt, MergeStmt, MergeWhen, OnConflictAction, OrderBy, Projection, SelectStmt,
        Statement, UpdateStmt, WindowSpec, CTE,
    },
    SQLError,
};
use uqa_core::memory::ProductionControl;

pub(super) struct Visitor<'a, 'c> {
    pub control: ProductionControl<'c>,
    pub visit: &'a mut dyn FnMut(&Expr) -> Result<(), SQLError>,
}

impl Visitor<'_, '_> {
    pub fn statement(&mut self, input: &Statement) -> Result<(), SQLError> {
        self.control.check()?;
        match input {
            Statement::Select(query) => self.query(query),
            Statement::Insert(insert) => self.insert(insert),
            Statement::Update(update) => self.update(update),
            Statement::Delete(delete) => self.delete(delete),
            Statement::Merge(merge) => self.merge(merge),
            Statement::Values { rows } => self.rows(rows),
            Statement::CreateTableAs { body, .. }
            | Statement::CreateMaterializedView { body, .. } => self.query(body),
            Statement::Explain { body, .. } => self.statement(body),
            Statement::DeclareCursor(cursor) => self.query(&cursor.query),
            Statement::Call { args, .. } => self.expressions(args),
            _ => Ok(()),
        }
    }

    fn expression(&mut self, input: &Expr) -> Result<(), SQLError> {
        self.control.check()?;
        (self.visit)(input)?;
        match input {
            Expr::Column(_)
            | Expr::QualifiedColumn { .. }
            | Expr::Param(_)
            | Expr::InternalColumn(_)
            | Expr::QualifiedStar(_)
            | Expr::Default
            | Expr::Literal(_)
            | Expr::TypedLiteral { .. }
            | Expr::RetainedLiteral(_)
            | Expr::Star => Ok(()),
            Expr::Func {
                args,
                order_by,
                filter,
                ..
            } => {
                self.expressions(args)?;
                self.orders(order_by)?;
                self.optional(filter.as_deref())
            }
            Expr::Array(items)
            | Expr::Row(items)
            | Expr::CompositeRow { items, .. }
            | Expr::And(items)
            | Expr::Or(items) => self.expressions(items),
            Expr::Binary { lhs, rhs, .. } => {
                self.expression(lhs)?;
                self.expression(rhs)
            }
            Expr::UnaryMinus(expr)
            | Expr::Not(expr)
            | Expr::IsNull { expr, .. }
            | Expr::Cast { expr, .. } => self.expression(expr),
            Expr::Between { expr, low, high } => {
                self.expression(expr)?;
                self.expression(low)?;
                self.expression(high)
            }
            Expr::InList { expr, list, .. } => {
                self.expression(expr)?;
                self.expressions(list)
            }
            Expr::WindowCall {
                args, spec, filter, ..
            } => {
                self.expressions(args)?;
                self.window(spec)?;
                self.optional(filter.as_deref())
            }
            Expr::Case {
                base,
                when,
                else_branch,
            } => {
                self.optional(base.as_deref())?;
                for (condition, value) in when {
                    self.expression(condition)?;
                    self.expression(value)?;
                }
                self.optional(else_branch.as_deref())
            }
            Expr::ScalarSubquery(query) | Expr::Exists { body: query, .. } => self.query(query),
            Expr::InSubquery { expr, body, .. } => {
                self.expression(expr)?;
                self.query(body)
            }
        }
    }

    fn expressions(&mut self, items: &[Expr]) -> Result<(), SQLError> {
        for item in items {
            self.expression(item)?;
        }
        Ok(())
    }
    fn optional(&mut self, input: Option<&Expr>) -> Result<(), SQLError> {
        if let Some(input) = input {
            self.expression(input)?;
        }
        Ok(())
    }
    fn rows(&mut self, rows: &[Vec<Expr>]) -> Result<(), SQLError> {
        for row in rows {
            self.expressions(row)?;
        }
        Ok(())
    }
    fn orders(&mut self, orders: &[OrderBy]) -> Result<(), SQLError> {
        for order in orders {
            self.expression(&order.expr)?;
        }
        Ok(())
    }
    fn projections(&mut self, projections: &[Projection]) -> Result<(), SQLError> {
        for projection in projections {
            self.expression(&projection.expr)?;
        }
        Ok(())
    }
    fn targets(&mut self, targets: &[AssignmentTarget]) -> Result<(), SQLError> {
        for target in targets {
            for expr in target.expressions() {
                self.expression(expr)?;
            }
        }
        Ok(())
    }
    fn assignments(&mut self, assignments: &[(AssignmentTargets, Expr)]) -> Result<(), SQLError> {
        for (targets, expression) in assignments {
            for target in targets.targets() {
                for expr in target.expressions() {
                    self.expression(expr)?;
                }
            }
            self.expression(expression)?;
        }
        Ok(())
    }
    fn window(&mut self, window: &WindowSpec) -> Result<(), SQLError> {
        self.expressions(&window.partition_by)?;
        self.orders(&window.order_by)?;
        if let Some(frame) = &window.frame {
            for bound in [&frame.start, &frame.end] {
                match bound {
                    FrameBound::Preceding(expr) | FrameBound::Following(expr) => {
                        self.expression(expr)?;
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }
    fn ctes(&mut self, ctes: &[CTE]) -> Result<(), SQLError> {
        for cte in ctes {
            self.control.check()?;
            if let Some(cycle) = &cte.cycle {
                self.expression(&cycle.mark_value)?;
                self.expression(&cycle.mark_default)?;
            }
            match &cte.body {
                CteBody::Query(query) => self.query(query)?,
                CteBody::Insert(insert) => self.insert(insert)?,
                CteBody::Update(update) => self.update(update)?,
                CteBody::Delete(delete) => self.delete(delete)?,
                CteBody::Merge(merge) => self.merge(merge)?,
            }
        }
        Ok(())
    }
    fn query(&mut self, query: &SelectStmt) -> Result<(), SQLError> {
        self.control.check()?;
        for window in &query.windows {
            self.window(&window.spec)?;
        }
        self.projections(&query.projections)?;
        self.rows(&query.values)?;
        if let Some(from) = &query.from {
            self.source(from)?;
        }
        self.optional(query.r#where.as_ref())?;
        self.expressions(&query.group_by)?;
        self.rows(&query.grouping_sets)?;
        self.optional(query.having.as_ref())?;
        self.orders(&query.order_by)?;
        self.optional(query.limit.as_ref())?;
        self.optional(query.offset.as_ref())?;
        self.ctes(&query.with)?;
        if let Some(set) = &query.set_op {
            if let Some(left) = &set.left {
                self.query(left)?;
            }
            self.query(&set.right)?;
            self.orders(&set.combined_order_by)?;
            self.optional(set.combined_limit.as_ref())?;
            self.optional(set.combined_offset.as_ref())?;
        }
        self.expressions(&query.distinct_on)
    }
    fn source(&mut self, source: &FromClause) -> Result<(), SQLError> {
        self.control.check()?;
        match source {
            FromClause::RetainedRows(_) | FromClause::Table { .. } => Ok(()),
            FromClause::Join {
                left, right, on, ..
            } => {
                self.source(left)?;
                self.source(right)?;
                self.optional(on.as_ref())
            }
            FromClause::Values { rows, .. } => self.rows(rows),
            FromClause::Function { args, .. } => self.expressions(args),
            FromClause::FunctionGroup { functions, .. } => {
                for function in functions {
                    self.expressions(&function.args)?;
                }
                Ok(())
            }
            FromClause::Subquery { body, .. } => self.query(body),
        }
    }
    fn insert(&mut self, insert: &InsertStmt) -> Result<(), SQLError> {
        self.targets(&insert.columns)?;
        self.ctes(&insert.with)?;
        self.rows(&insert.rows)?;
        if let Some(query) = &insert.select_source {
            self.query(query)?;
        }
        if let Some(conflict) = &insert.on_conflict {
            self.optional(conflict.predicate.as_deref())?;
            self.expressions(&conflict.expressions)?;
            if let OnConflictAction::Update {
                assignments,
                r#where,
            } = &conflict.action
            {
                self.assignments(assignments)?;
                self.optional(r#where.as_deref())?;
            }
        }
        self.projections(&insert.returning)
    }
    fn update(&mut self, update: &UpdateStmt) -> Result<(), SQLError> {
        self.assignments(&update.assignments)?;
        self.optional(update.r#where.as_ref())?;
        self.ctes(&update.with)?;
        if let Some(source) = &update.from {
            self.source(source)?;
        }
        self.projections(&update.returning)
    }
    fn delete(&mut self, delete: &DeleteStmt) -> Result<(), SQLError> {
        self.optional(delete.r#where.as_ref())?;
        self.ctes(&delete.with)?;
        if let Some(source) = &delete.using {
            self.source(source)?;
        }
        self.projections(&delete.returning)
    }
    fn merge(&mut self, merge: &MergeStmt) -> Result<(), SQLError> {
        self.ctes(&merge.with)?;
        self.source(&merge.source)?;
        self.expression(&merge.join_condition)?;
        for clause in &merge.when_clauses {
            match clause {
                MergeWhen::UpdateMatched {
                    condition,
                    assignments,
                }
                | MergeWhen::UpdateNotMatchedBySource {
                    condition,
                    assignments,
                } => {
                    self.optional(condition.as_ref())?;
                    self.assignments(assignments)?;
                }
                MergeWhen::InsertNotMatched {
                    condition,
                    columns,
                    values,
                    ..
                } => {
                    self.optional(condition.as_ref())?;
                    self.targets(columns)?;
                    self.expressions(values)?;
                }
                MergeWhen::DeleteMatched { condition }
                | MergeWhen::DeleteNotMatchedBySource { condition }
                | MergeWhen::NothingMatched { condition }
                | MergeWhen::NothingNotMatched { condition }
                | MergeWhen::NothingNotMatchedBySource { condition } => {
                    self.optional(condition.as_ref())?;
                }
            }
        }
        self.projections(&merge.returning)
    }
}
