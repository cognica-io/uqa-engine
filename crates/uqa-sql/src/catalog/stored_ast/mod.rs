//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Traversal and identity binding for durable SQL syntax trees.

use crate::{
    ast::{Expr, FrameBound, FromClause, SelectStmt, Statement},
    SQLError,
};
use std::collections::BTreeSet;
use uqa_core::RelationIdentity;
mod expressions;
mod merge;
mod routines;
mod sources;
mod types;
pub use expressions::*;
pub use merge::visit_stored_statement_merges;
pub use routines::*;
pub use sources::*;
pub use types::*;

pub type MergeCallback<'a> = &'a mut dyn FnMut(&mut crate::ast::MergeStmt) -> Result<(), SQLError>;

pub type ExpressionCallback<'a> = &'a mut dyn FnMut(&mut Expr) -> Result<(), SQLError>;
pub type SourceCallback<'a> = &'a mut dyn FnMut(&mut FromClause) -> Result<(), SQLError>;

pub struct StoredAstVisitor<'a, R, F> {
    pub source: Option<SourceCallback<'a>>,
    pub merge: Option<MergeCallback<'a>>,
    pub expression: Option<ExpressionCallback<'a>>,
    pub ty: Option<&'a mut dyn FnMut(&mut String)>,
    pub relation: &'a mut R,
    pub routine: &'a mut F,
}

impl<R, F> StoredAstVisitor<'_, R, F>
where
    R: FnMut(&mut String) -> Result<(), SQLError>,
    F: FnMut(&mut String, Option<&mut Option<crate::ast::FunctionBinding>>) -> Result<(), SQLError>,
{
    pub fn bind_statement(&mut self, statement: &mut Statement) -> Result<(), SQLError> {
        let ctes = BTreeSet::new();
        match statement {
            Statement::Select(query) => self.bind_select(query, &ctes),
            Statement::Insert(insert) => self.bind_insert(insert, &ctes),
            Statement::Update(update) => self.bind_update(update, &ctes),
            Statement::Delete(delete) => self.bind_delete(delete, &ctes),
            Statement::Notify { .. } => Ok(()),
            Statement::Values { rows } => {
                for expression in rows.iter_mut().flatten() {
                    self.bind_expr(expression, &ctes)?;
                }
                Ok(())
            }
            Statement::Merge(merge) => self.bind_merge(merge, &ctes),
            _ => Err(SQLError::Internal(
                "catalog-owned statement has an unsupported dependency shape".into(),
            )),
        }
    }

    fn bind_insert(
        &mut self,
        insert: &mut crate::ast::InsertStmt,
        inherited: &BTreeSet<String>,
    ) -> Result<(), SQLError> {
        (self.relation)(&mut insert.table)?;
        let visible = self.bind_ctes(&mut insert.with, inherited)?;
        if let Some(source) = insert.select_source.as_deref_mut() {
            self.bind_select(source, &visible)?;
        }
        for expression in insert.rows.iter_mut().flatten() {
            self.bind_expr(expression, &visible)?;
        }
        if let Some(conflict) = &mut insert.on_conflict {
            for expression in &mut conflict.expressions {
                self.bind_expr(expression, &visible)?;
            }
            if let Some(predicate) = conflict.predicate.as_deref_mut() {
                self.bind_expr(predicate, &visible)?;
            }
            if let crate::ast::OnConflictAction::Update {
                assignments,
                r#where,
            } = &mut conflict.action
            {
                for (_, expression) in assignments {
                    self.bind_expr(expression, &visible)?;
                }
                if let Some(expression) = r#where {
                    self.bind_expr(expression, &visible)?;
                }
            }
        }
        for projection in &mut insert.returning {
            self.bind_expr(&mut projection.expr, &visible)?;
        }
        Ok(())
    }

    fn bind_update(
        &mut self,
        update: &mut crate::ast::UpdateStmt,
        inherited: &BTreeSet<String>,
    ) -> Result<(), SQLError> {
        (self.relation)(&mut update.table)?;
        let visible = self.bind_ctes(&mut update.with, inherited)?;
        if let Some(source) = &mut update.from {
            self.bind_from(source, &visible)?;
        }
        for (_, expression) in &mut update.assignments {
            self.bind_expr(expression, &visible)?;
        }
        if let Some(expression) = &mut update.r#where {
            self.bind_expr(expression, &visible)?;
        }
        for projection in &mut update.returning {
            self.bind_expr(&mut projection.expr, &visible)?;
        }
        Ok(())
    }

    fn bind_delete(
        &mut self,
        delete: &mut crate::ast::DeleteStmt,
        inherited: &BTreeSet<String>,
    ) -> Result<(), SQLError> {
        (self.relation)(&mut delete.table)?;
        let visible = self.bind_ctes(&mut delete.with, inherited)?;
        if let Some(source) = &mut delete.using {
            self.bind_from(source, &visible)?;
        }
        if let Some(expression) = &mut delete.r#where {
            self.bind_expr(expression, &visible)?;
        }
        for projection in &mut delete.returning {
            self.bind_expr(&mut projection.expr, &visible)?;
        }
        Ok(())
    }

    fn bind_ctes(
        &mut self,
        ctes: &mut [crate::ast::CTE],
        inherited: &BTreeSet<String>,
    ) -> Result<BTreeSet<String>, SQLError> {
        let mut visible = inherited.clone();
        let recursive = ctes.iter().any(|cte| cte.recursive).then(|| {
            ctes.iter()
                .map(|cte| cte.name.clone())
                .collect::<BTreeSet<_>>()
        });
        for cte in ctes {
            let body_scope = recursive.as_ref().map_or_else(
                || visible.clone(),
                |recursive| inherited.union(recursive).cloned().collect(),
            );
            match &mut cte.body {
                crate::ast::CteBody::Query(query) => self.bind_select(query, &body_scope)?,
                crate::ast::CteBody::Insert(plan) => self.bind_insert(plan, &body_scope)?,
                crate::ast::CteBody::Update(plan) => self.bind_update(plan, &body_scope)?,
                crate::ast::CteBody::Delete(plan) => self.bind_delete(plan, &body_scope)?,
                crate::ast::CteBody::Merge(plan) => self.bind_merge(plan, &body_scope)?,
            }
            if let Some(cycle) = &mut cte.cycle {
                self.bind_expr(&mut cycle.mark_value, &body_scope)?;
                self.bind_expr(&mut cycle.mark_default, &body_scope)?;
            }
            visible.insert(cte.name.clone());
        }
        Ok(visible)
    }

    fn bind_select(
        &mut self,
        select: &mut SelectStmt,
        inherited: &BTreeSet<String>,
    ) -> Result<(), SQLError> {
        let visible = self.bind_ctes(&mut select.with, inherited)?;
        if let Some(source) = &mut select.from {
            self.bind_from(source, &visible)?;
        }
        for projection in &mut select.projections {
            self.bind_expr(&mut projection.expr, &visible)?;
        }
        for expression in select.values.iter_mut().flatten() {
            self.bind_expr(expression, &visible)?;
        }
        if let Some(expression) = &mut select.r#where {
            self.bind_expr(expression, &visible)?;
        }
        for expression in &mut select.group_by {
            self.bind_expr(expression, &visible)?;
        }
        for expression in select.grouping_sets.iter_mut().flatten() {
            self.bind_expr(expression, &visible)?;
        }
        if let Some(expression) = &mut select.having {
            self.bind_expr(expression, &visible)?;
        }
        for order in &mut select.order_by {
            self.bind_expr(&mut order.expr, &visible)?;
        }
        if let Some(expression) = &mut select.limit {
            self.bind_expr(expression, &visible)?;
        }
        if let Some(expression) = &mut select.offset {
            self.bind_expr(expression, &visible)?;
        }
        for expression in &mut select.distinct_on {
            self.bind_expr(expression, &visible)?;
        }
        if let Some(set) = &mut select.set_op {
            if let Some(left) = &mut set.left {
                self.bind_select(left, &visible)?;
            }
            self.bind_select(&mut set.right, &visible)?;
            for order in &mut set.combined_order_by {
                self.bind_expr(&mut order.expr, &visible)?;
            }
            if let Some(expression) = &mut set.combined_limit {
                self.bind_expr(expression, &visible)?;
            }
            if let Some(expression) = &mut set.combined_offset {
                self.bind_expr(expression, &visible)?;
            }
        }
        Ok(())
    }

    fn bind_from(
        &mut self,
        source: &mut FromClause,
        visible_ctes: &BTreeSet<String>,
    ) -> Result<(), SQLError> {
        if let FromClause::Table { name, .. } = source {
            let is_cte =
                RelationIdentity::parse_reference(name)
                    .ok()
                    .is_some_and(|(schema, relation)| {
                        schema.is_none() && visible_ctes.contains(&relation)
                    });
            if is_cte {
                return Ok(());
            }
        }
        if let Some(visit) = self.source.as_mut() {
            visit(source)?;
        }
        match source {
            FromClause::Table { name, .. } => {
                (self.relation)(name)?;
            }
            FromClause::Join {
                left, right, on, ..
            } => {
                self.bind_from(left, visible_ctes)?;
                self.bind_from(right, visible_ctes)?;
                if let Some(expression) = on {
                    self.bind_expr(expression, visible_ctes)?;
                }
            }
            FromClause::Values { rows, .. } => {
                for expression in rows.iter_mut().flatten() {
                    self.bind_expr(expression, visible_ctes)?;
                }
            }
            FromClause::Function {
                name,
                binding,
                relations,
                args,
                ..
            } => {
                (self.routine)(name, Some(binding))?;
                if let Some(relations) = relations {
                    (self.relation)(&mut relations.left)?;
                    (self.relation)(&mut relations.right)?;
                }
                for expression in args {
                    self.bind_expr(expression, visible_ctes)?;
                }
            }
            FromClause::FunctionGroup { functions, .. } => {
                for function in functions {
                    (self.routine)(&mut function.name, Some(&mut function.binding))?;
                    if let Some(relations) = &mut function.relations {
                        (self.relation)(&mut relations.left)?;
                        (self.relation)(&mut relations.right)?;
                    }
                    for expression in &mut function.args {
                        self.bind_expr(expression, visible_ctes)?;
                    }
                }
            }
            FromClause::Subquery { body, .. } => self.bind_select(body, visible_ctes)?,
        }
        Ok(())
    }

    fn bind_expression_type(&mut self, expression: &mut Expr) -> Result<(), SQLError> {
        if let Some(visit) = self.expression.as_mut() {
            visit(expression)?;
        }
        if let (Some(visit), Expr::Cast { ty, .. } | Expr::TypedLiteral { ty, .. }) =
            (self.ty.as_mut(), expression)
        {
            visit(ty);
        }
        Ok(())
    }

    pub fn bind_expr(
        &mut self,
        expression: &mut Expr,
        visible_ctes: &BTreeSet<String>,
    ) -> Result<(), SQLError> {
        self.bind_expression_type(expression)?;
        match expression {
            Expr::Func {
                name,
                binding,
                args,
                order_by,
                filter,
                ..
            } => {
                for argument in args {
                    self.bind_expr(argument, visible_ctes)?;
                }
                for order in order_by {
                    self.bind_expr(&mut order.expr, visible_ctes)?;
                }
                if let Some(filter) = filter {
                    self.bind_expr(filter, visible_ctes)?;
                }
                (self.routine)(name, Some(binding))?;
            }
            Expr::Array(items) | Expr::Row(items) | Expr::And(items) | Expr::Or(items) => {
                for item in items {
                    self.bind_expr(item, visible_ctes)?;
                }
            }
            Expr::Binary { lhs, rhs, .. } => {
                self.bind_expr(lhs, visible_ctes)?;
                self.bind_expr(rhs, visible_ctes)?;
            }
            Expr::UnaryMinus(inner)
            | Expr::Not(inner)
            | Expr::IsNull { expr: inner, .. }
            | Expr::Cast { expr: inner, .. } => self.bind_expr(inner, visible_ctes)?,
            Expr::Between { expr, low, high } => {
                self.bind_expr(expr, visible_ctes)?;
                self.bind_expr(low, visible_ctes)?;
                self.bind_expr(high, visible_ctes)?;
            }
            Expr::InList { expr, list, .. } => {
                self.bind_expr(expr, visible_ctes)?;
                for item in list {
                    self.bind_expr(item, visible_ctes)?;
                }
            }
            Expr::WindowCall { name, args, spec } => {
                for argument in args {
                    self.bind_expr(argument, visible_ctes)?;
                }
                for partition in &mut spec.partition_by {
                    self.bind_expr(partition, visible_ctes)?;
                }
                for order in &mut spec.order_by {
                    self.bind_expr(&mut order.expr, visible_ctes)?;
                }
                if let Some(frame) = &mut spec.frame {
                    for bound in [&mut frame.start, &mut frame.end] {
                        if let FrameBound::Preceding(inner) | FrameBound::Following(inner) = bound {
                            self.bind_expr(inner, visible_ctes)?;
                        }
                    }
                }
                (self.routine)(name, None)?;
            }
            Expr::Case {
                base,
                when,
                else_branch,
            } => {
                if let Some(base) = base {
                    self.bind_expr(base, visible_ctes)?;
                }
                for (condition, result) in when {
                    self.bind_expr(condition, visible_ctes)?;
                    self.bind_expr(result, visible_ctes)?;
                }
                if let Some(branch) = else_branch {
                    self.bind_expr(branch, visible_ctes)?;
                }
            }
            Expr::ScalarSubquery(body) | Expr::Exists { body, .. } => {
                self.bind_select(body, visible_ctes)?;
            }
            Expr::InSubquery { expr, body, .. } => {
                self.bind_expr(expr, visible_ctes)?;
                self.bind_select(body, visible_ctes)?;
            }
            Expr::Star
            | Expr::QualifiedStar(_)
            | Expr::Default
            | Expr::Column(_)
            | Expr::QualifiedColumn { .. }
            | Expr::InternalColumn(_)
            | Expr::Literal(_)
            | Expr::TypedLiteral { .. }
            | Expr::Param(_) => {}
        }
        Ok(())
    }
}
