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
mod input;
mod literals;
mod merge;
mod routines;
mod sites;
mod sources;
mod types;
use crate::catalog::work::Work;
pub use expressions::*;
use input::{Actions, Reader};
pub use literals::read_unknown_stored_literal;
pub use merge::visit_stored_statement_merges;
pub use routines::*;
pub use sites::*;
pub use sources::*;
pub use types::*;

pub type MergeCallback<'a> = &'a mut dyn FnMut(&mut crate::ast::MergeStmt) -> Result<(), SQLError>;

pub type ExpressionCallback<'a> = &'a mut dyn FnMut(&mut Expr) -> Result<(), SQLError>;
pub type ProjectionCallback<'a> =
    &'a mut dyn FnMut(&mut crate::ast::Projection) -> Result<(), SQLError>;
pub type SourceCallback<'a> = &'a mut dyn FnMut(&mut FromClause) -> Result<(), SQLError>;

pub struct StoredAstVisitor<'a, R, F> {
    pub source: Option<SourceCallback<'a>>,
    pub merge: Option<MergeCallback<'a>>,
    pub expression: Option<ExpressionCallback<'a>>,
    /// Called for each item of a select list or `RETURNING` list before its expression is visited.
    pub projection: Option<ProjectionCallback<'a>>,
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
        let control = uqa_core::memory::ProductionControl::uncontrolled();
        crate::catalog::work::inline(Reader::new(self, &control, &mut ()).bind_statement(statement))
    }

    pub fn bind_expr(
        &mut self,
        expression: &mut Expr,
        visible_ctes: &BTreeSet<String>,
    ) -> Result<(), SQLError> {
        let control = uqa_core::memory::ProductionControl::uncontrolled();
        crate::catalog::work::inline(
            Reader::new(self, &control, &mut ()).bind_expr(expression, visible_ctes),
        )
    }
}

impl<W: Work + Send, A: Actions<W>> Reader<'_, '_, A, W> {
    pub(super) async fn bind_statement(
        &mut self,
        statement: &mut Statement,
    ) -> Result<(), SQLError> {
        self.checkpoint().await?;
        let ctes = BTreeSet::new();
        match statement {
            Statement::Select(query) => crate::catalog::work::nested!(
                W::MAY_SUSPEND,
                self.control,
                self.bind_select(query, &ctes)
            ),
            Statement::Insert(insert) => self.bind_insert(insert, &ctes).await,
            Statement::Update(update) => self.bind_update(update, &ctes).await,
            Statement::Delete(delete) => self.bind_delete(delete, &ctes).await,
            Statement::Notify { .. } => Ok(()),
            Statement::Values { rows } => {
                for expression in rows.iter_mut().flatten() {
                    self.bind_expr(expression, &ctes).await?;
                }
                Ok(())
            }
            Statement::Merge(merge) => self.bind_merge(merge, &ctes).await,
            _ => Err(SQLError::Internal(
                "catalog-owned statement has an unsupported dependency shape".into(),
            )),
        }
    }

    async fn bind_targets(
        &mut self,
        targets: &mut [crate::ast::AssignmentTarget],
        visible: &BTreeSet<String>,
    ) -> Result<(), SQLError> {
        for target in targets {
            for expression in target.expressions_mut() {
                self.bind_expr(expression, visible).await?;
            }
        }
        Ok(())
    }

    async fn bind_assignments(
        &mut self,
        assignments: &mut [(crate::ast::AssignmentTargets, Expr)],
        visible: &BTreeSet<String>,
    ) -> Result<(), SQLError> {
        for (target, value) in assignments {
            self.bind_targets(target.targets_mut(), visible).await?;
            self.bind_expr(value, visible).await?;
        }
        Ok(())
    }

    async fn bind_insert(
        &mut self,
        insert: &mut crate::ast::InsertStmt,
        inherited: &BTreeSet<String>,
    ) -> Result<(), SQLError> {
        self.actions.relation(&mut insert.table)?;
        let visible = self.bind_ctes(&mut insert.with, inherited).await?;
        if let Some(source) = insert.select_source.as_deref_mut() {
            crate::catalog::work::nested!(
                W::MAY_SUSPEND,
                self.control,
                self.bind_select(source, &visible)
            )?;
        }
        self.bind_targets(&mut insert.columns, &visible).await?;
        for expression in insert.rows.iter_mut().flatten() {
            self.bind_expr(expression, &visible).await?;
        }
        if let Some(conflict) = &mut insert.on_conflict {
            for expression in &mut conflict.expressions {
                self.bind_expr(expression, &visible).await?;
            }
            if let Some(predicate) = conflict.predicate.as_deref_mut() {
                self.bind_expr(predicate, &visible).await?;
            }
            if let crate::ast::OnConflictAction::Update {
                assignments,
                r#where,
            } = &mut conflict.action
            {
                self.bind_assignments(assignments, &visible).await?;
                if let Some(expression) = r#where {
                    self.bind_expr(expression, &visible).await?;
                }
            }
        }
        for projection in &mut insert.returning {
            self.bind_projection(projection, &visible).await?;
        }
        Ok(())
    }

    async fn bind_update(
        &mut self,
        update: &mut crate::ast::UpdateStmt,
        inherited: &BTreeSet<String>,
    ) -> Result<(), SQLError> {
        self.actions.relation(&mut update.table)?;
        let visible = self.bind_ctes(&mut update.with, inherited).await?;
        if let Some(source) = &mut update.from {
            self.bind_from(source, &visible).await?;
        }
        self.bind_assignments(&mut update.assignments, &visible)
            .await?;
        if let Some(expression) = &mut update.r#where {
            self.bind_expr(expression, &visible).await?;
        }
        for projection in &mut update.returning {
            self.bind_projection(projection, &visible).await?;
        }
        Ok(())
    }

    async fn bind_delete(
        &mut self,
        delete: &mut crate::ast::DeleteStmt,
        inherited: &BTreeSet<String>,
    ) -> Result<(), SQLError> {
        self.actions.relation(&mut delete.table)?;
        let visible = self.bind_ctes(&mut delete.with, inherited).await?;
        if let Some(source) = &mut delete.using {
            self.bind_from(source, &visible).await?;
        }
        if let Some(expression) = &mut delete.r#where {
            self.bind_expr(expression, &visible).await?;
        }
        for projection in &mut delete.returning {
            self.bind_projection(projection, &visible).await?;
        }
        Ok(())
    }

    async fn bind_ctes(
        &mut self,
        ctes: &mut [crate::ast::CTE],
        inherited: &BTreeSet<String>,
    ) -> Result<BTreeSet<String>, SQLError> {
        self.checkpoint().await?;
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
                crate::ast::CteBody::Query(query) => crate::catalog::work::nested!(
                    W::MAY_SUSPEND,
                    self.control,
                    self.bind_select(query, &body_scope)
                )?,
                crate::ast::CteBody::Insert(plan) => crate::catalog::work::nested!(
                    W::MAY_SUSPEND,
                    self.control,
                    self.bind_insert(plan, &body_scope)
                )?,
                crate::ast::CteBody::Update(plan) => crate::catalog::work::nested!(
                    W::MAY_SUSPEND,
                    self.control,
                    self.bind_update(plan, &body_scope)
                )?,
                crate::ast::CteBody::Delete(plan) => crate::catalog::work::nested!(
                    W::MAY_SUSPEND,
                    self.control,
                    self.bind_delete(plan, &body_scope)
                )?,
                crate::ast::CteBody::Merge(plan) => crate::catalog::work::nested!(
                    W::MAY_SUSPEND,
                    self.control,
                    self.bind_merge(plan, &body_scope)
                )?,
            }
            if let Some(cycle) = &mut cte.cycle {
                self.bind_expr(&mut cycle.mark_value, &body_scope).await?;
                self.bind_expr(&mut cycle.mark_default, &body_scope).await?;
            }
            visible.insert(cte.name.clone());
        }
        Ok(visible)
    }

    async fn bind_select(
        &mut self,
        select: &mut SelectStmt,
        inherited: &BTreeSet<String>,
    ) -> Result<(), SQLError> {
        self.checkpoint().await?;
        let visible = self.bind_ctes(&mut select.with, inherited).await?;
        if let Some(set) = select.set_op.as_mut().filter(|set| set.left.is_some()) {
            self.bind_set_operation(set, &visible).await?;
            let left = set
                .left
                .as_deref()
                .expect("explicit set-operation left input");
            // These fields mirror the left input for AST compatibility; they are not another syntax occurrence.
            select.projections.clone_from(&left.projections);
            select.values.clone_from(&left.values);
            select.from.clone_from(&left.from);
            select.r#where.clone_from(&left.r#where);
            select.group_by.clone_from(&left.group_by);
            select.order_by.clone_from(&left.order_by);
            select.limit.clone_from(&left.limit);
            select.with_ties = left.with_ties;
            select.offset.clone_from(&left.offset);
            return Ok(());
        }
        if let Some(source) = &mut select.from {
            self.bind_from(source, &visible).await?;
        }
        for projection in &mut select.projections {
            self.bind_projection(projection, &visible).await?;
        }
        for expression in select.values.iter_mut().flatten() {
            self.bind_expr(expression, &visible).await?;
        }
        if let Some(expression) = &mut select.r#where {
            self.bind_expr(expression, &visible).await?;
        }
        for expression in &mut select.group_by {
            self.bind_expr(expression, &visible).await?;
        }
        for expression in select.grouping_sets.iter_mut().flatten() {
            self.bind_expr(expression, &visible).await?;
        }
        if let Some(expression) = &mut select.having {
            self.bind_expr(expression, &visible).await?;
        }
        for order in &mut select.order_by {
            self.bind_expr(&mut order.expr, &visible).await?;
        }
        if let Some(expression) = &mut select.limit {
            self.bind_expr(expression, &visible).await?;
        }
        if let Some(expression) = &mut select.offset {
            self.bind_expr(expression, &visible).await?;
        }
        for expression in &mut select.distinct_on {
            self.bind_expr(expression, &visible).await?;
        }
        for window in &mut select.windows {
            for expression in window.spec.expressions_mut() {
                self.bind_expr(expression, &visible).await?;
            }
        }
        if let Some(set) = &mut select.set_op {
            self.bind_set_operation(set, &visible).await?;
        }
        Ok(())
    }

    async fn bind_set_operation(
        &mut self,
        set: &mut crate::ast::SetOp,
        visible: &BTreeSet<String>,
    ) -> Result<(), SQLError> {
        if let Some(left) = &mut set.left {
            crate::catalog::work::nested!(
                W::MAY_SUSPEND,
                self.control,
                self.bind_select(left, visible)
            )?;
        }
        crate::catalog::work::nested!(
            W::MAY_SUSPEND,
            self.control,
            self.bind_select(&mut set.right, visible)
        )?;
        for order in &mut set.combined_order_by {
            self.bind_expr(&mut order.expr, visible).await?;
        }
        if let Some(expression) = &mut set.combined_limit {
            self.bind_expr(expression, visible).await?;
        }
        if let Some(expression) = &mut set.combined_offset {
            self.bind_expr(expression, visible).await?;
        }
        Ok(())
    }

    async fn bind_from(
        &mut self,
        source: &mut FromClause,
        visible_ctes: &BTreeSet<String>,
    ) -> Result<(), SQLError> {
        self.checkpoint().await?;
        if let FromClause::Table { name, .. } = source {
            let is_cte = RelationIdentity::parse_reference(name).is_ok_and(|(schema, relation)| {
                schema.is_none() && visible_ctes.contains(&relation)
            });
            if is_cte {
                return Ok(());
            }
        }
        self.actions.source(source)?;
        match source {
            crate::ast::FromClause::RetainedRows(_) => {
                return Err(SQLError::Internal(
                    "retained row source cannot be stored in a catalog statement".into(),
                ))
            }
            FromClause::Table { name, .. } => {
                self.actions.relation(name)?;
            }
            FromClause::Join {
                left, right, on, ..
            } => {
                crate::catalog::work::nested!(
                    W::MAY_SUSPEND,
                    self.control,
                    self.bind_from(left, visible_ctes)
                )?;
                crate::catalog::work::nested!(
                    W::MAY_SUSPEND,
                    self.control,
                    self.bind_from(right, visible_ctes)
                )?;
                if let Some(expression) = on {
                    self.bind_expr(expression, visible_ctes).await?;
                }
            }
            FromClause::Values { rows, .. } => {
                for expression in rows.iter_mut().flatten() {
                    self.bind_expr(expression, visible_ctes).await?;
                }
            }
            FromClause::Function {
                name,
                binding,
                relations,
                args,
                ..
            } => {
                self.actions
                    .routine(name, Some(binding), self.control, self.work)
                    .await?;
                if let Some(relations) = relations {
                    self.actions.relation(&mut relations.left)?;
                    self.actions.relation(&mut relations.right)?;
                }
                for expression in args {
                    self.bind_expr(expression, visible_ctes).await?;
                }
            }
            FromClause::FunctionGroup { functions, .. } => {
                for function in functions {
                    self.actions
                        .routine(
                            &mut function.name,
                            Some(&mut function.binding),
                            self.control,
                            self.work,
                        )
                        .await?;
                    if let Some(relations) = &mut function.relations {
                        self.actions.relation(&mut relations.left)?;
                        self.actions.relation(&mut relations.right)?;
                    }
                    for expression in &mut function.args {
                        self.bind_expr(expression, visible_ctes).await?;
                    }
                }
            }
            FromClause::Subquery { body, .. } => crate::catalog::work::nested!(
                W::MAY_SUSPEND,
                self.control,
                self.bind_select(body, visible_ctes)
            )?,
        }
        Ok(())
    }

    async fn bind_expression_type(&mut self, expression: &mut Expr) -> Result<(), SQLError> {
        if let Expr::RetainedLiteral(literal) = expression {
            *expression = literal.materialize()?;
        }
        self.actions
            .expression(expression, self.control, self.work)
            .await?;
        if let Expr::Cast { ty, .. }
        | Expr::TypedLiteral { ty, .. }
        | Expr::CompositeRow {
            binding: crate::ast::CompositeRowBinding { ty, .. },
            ..
        } = expression
        {
            self.actions.ty(ty);
        }
        Ok(())
    }

    /// Bind a window call's arguments, `FILTER`, keys and frame offsets.
    async fn bind_call_inputs(
        &mut self,
        args: &mut [Expr],
        order_by: &mut [crate::ast::OrderBy],
        filter: Option<&mut Expr>,
        visible_ctes: &BTreeSet<String>,
    ) -> Result<(), SQLError> {
        for argument in args {
            self.bind_expr(argument, visible_ctes).await?;
        }
        for order in order_by {
            self.bind_expr(&mut order.expr, visible_ctes).await?;
        }
        if let Some(filter) = filter {
            self.bind_expr(filter, visible_ctes).await?;
        }
        Ok(())
    }

    async fn bind_window_call(
        &mut self,
        args: &mut [Expr],
        filter: Option<&mut Expr>,
        spec: &mut crate::ast::WindowSpec,
        visible_ctes: &BTreeSet<String>,
    ) -> Result<(), SQLError> {
        self.bind_call_inputs(args, &mut [], filter, visible_ctes)
            .await?;
        for partition in &mut spec.partition_by {
            self.bind_expr(partition, visible_ctes).await?;
        }
        for order in &mut spec.order_by {
            self.bind_expr(&mut order.expr, visible_ctes).await?;
        }
        if let Some(frame) = &mut spec.frame {
            for bound in [&mut frame.start, &mut frame.end] {
                if let FrameBound::Preceding(inner) | FrameBound::Following(inner) = bound {
                    self.bind_expr(inner, visible_ctes).await?;
                }
            }
        }
        Ok(())
    }

    async fn bind_projection(
        &mut self,
        projection: &mut crate::ast::Projection,
        visible_ctes: &BTreeSet<String>,
    ) -> Result<(), SQLError> {
        self.actions.projection(projection)?;
        self.bind_expr(&mut projection.expr, visible_ctes).await
    }

    pub(super) async fn bind_expr(
        &mut self,
        expression: &mut Expr,
        visible_ctes: &BTreeSet<String>,
    ) -> Result<(), SQLError> {
        self.checkpoint().await?;
        self.bind_expression_type(expression).await?;
        if matches!(
            expression,
            Expr::Star
                | Expr::QualifiedStar(_)
                | Expr::Default
                | Expr::Column(_)
                | Expr::QualifiedColumn { .. }
                | Expr::InternalColumn(_)
                | Expr::Literal(_)
                | Expr::TypedLiteral { .. }
                | Expr::RetainedLiteral(_)
                | Expr::Param(_)
        ) {
            return Ok(());
        }
        crate::catalog::work::nested!(
            W::MAY_SUSPEND,
            self.control,
            self.bind_descendants(expression, visible_ctes)
        )
    }

    async fn bind_case(
        &mut self,
        base: Option<&mut Expr>,
        arms: &mut [(Expr, Expr)],
        otherwise: Option<&mut Expr>,
        visible: &BTreeSet<String>,
    ) -> Result<(), SQLError> {
        if let Some(value) = base {
            self.bind_expr(value, visible).await?;
        }
        for (condition, result) in arms {
            self.bind_expr(condition, visible).await?;
            self.bind_expr(result, visible).await?;
        }
        if let Some(value) = otherwise {
            self.bind_expr(value, visible).await?;
        }
        Ok(())
    }

    async fn bind_descendants(
        &mut self,
        expression: &mut Expr,
        visible_ctes: &BTreeSet<String>,
    ) -> Result<(), SQLError> {
        match expression {
            Expr::Func {
                name,
                binding,
                args,
                order_by,
                filter,
                ..
            } => {
                self.bind_call_inputs(args, order_by, filter.as_deref_mut(), visible_ctes)
                    .await?;
                self.actions
                    .routine(name, Some(binding), self.control, self.work)
                    .await?;
            }
            Expr::Array(items)
            | Expr::Row(items)
            | Expr::CompositeRow { items, .. }
            | Expr::And(items)
            | Expr::Or(items) => {
                for item in items {
                    self.bind_expr(item, visible_ctes).await?;
                }
            }
            Expr::Binary { lhs, rhs, .. } => {
                self.bind_expr(lhs, visible_ctes).await?;
                self.bind_expr(rhs, visible_ctes).await?;
            }
            Expr::UnaryMinus(inner)
            | Expr::Not(inner)
            | Expr::IsNull { expr: inner, .. }
            | Expr::Cast { expr: inner, .. } => self.bind_expr(inner, visible_ctes).await?,
            Expr::Between { expr, low, high } => {
                self.bind_expr(expr, visible_ctes).await?;
                self.bind_expr(low, visible_ctes).await?;
                self.bind_expr(high, visible_ctes).await?;
            }
            Expr::InList { expr, list, .. } => {
                self.bind_expr(expr, visible_ctes).await?;
                for item in list {
                    self.bind_expr(item, visible_ctes).await?;
                }
            }
            Expr::WindowCall {
                name,
                args,
                spec,
                filter,
                ..
            } => {
                self.bind_window_call(args, filter.as_deref_mut(), spec, visible_ctes)
                    .await?;
                self.actions
                    .routine(name, None, self.control, self.work)
                    .await?;
            }
            Expr::Case {
                base,
                when,
                else_branch,
            } => {
                self.bind_case(
                    base.as_deref_mut(),
                    when,
                    else_branch.as_deref_mut(),
                    visible_ctes,
                )
                .await?;
            }
            Expr::ScalarSubquery(body) | Expr::Exists { body, .. } => {
                crate::catalog::work::nested!(
                    W::MAY_SUSPEND,
                    self.control,
                    self.bind_select(body, visible_ctes)
                )?;
            }
            Expr::InSubquery { expr, body, .. } => {
                self.bind_expr(expr, visible_ctes).await?;
                crate::catalog::work::nested!(
                    W::MAY_SUSPEND,
                    self.control,
                    self.bind_select(body, visible_ctes)
                )?;
            }
            Expr::Star
            | Expr::QualifiedStar(_)
            | Expr::Default
            | Expr::Column(_)
            | Expr::QualifiedColumn { .. }
            | Expr::InternalColumn(_)
            | Expr::Literal(_)
            | Expr::TypedLiteral { .. }
            | Expr::RetainedLiteral(_)
            | Expr::Param(_) => {}
        }
        Ok(())
    }
}
