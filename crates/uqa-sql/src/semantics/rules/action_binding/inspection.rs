//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read rule references without constructing a replacement action or expression tree.

use super::{
    collect_visible_scope, namespace, RuleActionBodyResolver, RuleBindingContext, RuleBindingScope,
    RuleSourceCatalog,
};
use crate::{
    ast::{Expr, FrameBound, FromClause, OrderBy, Projection, RuleEvent, SelectStmt, Statement},
    plpgsql::VariableResolver,
    SQLError,
};
use std::collections::BTreeSet;
use uqa_core::memory::ProductionControl;

mod statements;

pub(super) fn expression(
    expression: &Expr,
    resolver: &mut dyn VariableResolver,
    shadowed: &BTreeSet<String>,
) -> Result<(), SQLError> {
    expression_with_control(
        expression,
        resolver,
        shadowed,
        &ProductionControl::uncontrolled(),
    )
}

pub(super) fn expression_with_control(
    expression: &Expr,
    resolver: &mut dyn VariableResolver,
    shadowed: &BTreeSet<String>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    Inspector { resolver }.expression(
        expression,
        &RuleBindingScope::from_qualifiers_with_control(shadowed, control)?,
        &RuleBindingContext::from_control(control),
    )
}

pub(super) fn select(
    catalog: &dyn RuleSourceCatalog,
    select: &SelectStmt,
    resolver: &mut dyn VariableResolver,
) -> Result<(), SQLError> {
    Inspector { resolver }.select(
        select,
        &RuleBindingScope::default(),
        &RuleBindingContext::with_catalog(catalog, false),
    )
}

pub(super) fn action(
    catalog: &dyn RuleSourceCatalog,
    action: &Statement,
    action_columns: &BTreeSet<String>,
    resolver: &mut dyn VariableResolver,
) -> Result<(), SQLError> {
    action_with_control(
        catalog,
        action,
        action_columns,
        resolver,
        &ProductionControl::uncontrolled(),
    )
}

pub(super) fn action_with_control(
    catalog: &dyn RuleSourceCatalog,
    action: &Statement,
    action_columns: &dyn super::names::ActionColumns,
    resolver: &mut dyn VariableResolver,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    let context = RuleBindingContext::with_control(catalog, true, control);
    let (returning, aliases, action_event) = match action {
        Statement::Insert(input) => (
            &input.returning,
            &input.returning_aliases,
            RuleEvent::Insert,
        ),
        Statement::Update(input) => (
            &input.returning,
            &input.returning_aliases,
            RuleEvent::Update,
        ),
        Statement::Delete(input) => (
            &input.returning,
            &input.returning_aliases,
            RuleEvent::Delete,
        ),
        _ => {
            return Inspector { resolver }.statement(action, &RuleBindingScope::default(), &context)
        }
    };
    Inspector {
        resolver: &mut RuleActionBodyResolver {
            action_columns,
            action_event,
            event_resolver: resolver,
        },
    }
    .statement(action, &RuleBindingScope::default(), &context)?;
    // The action's RETURNING row images have a separate namespace from its event-row inputs.
    Inspector {
        resolver: &mut super::returning::RuleActionReturningResolver {
            action_columns,
            aliases,
            action_event,
            event_resolver: resolver,
        },
    }
    .projections(returning, &RuleBindingScope::default(), &context)
}

pub(super) fn scoped_expression(
    expression: &Expr,
    resolver: &mut dyn VariableResolver,
    scope: &RuleBindingScope,
    context: &RuleBindingContext<'_>,
) -> Result<(), SQLError> {
    Inspector { resolver }.expression(expression, scope, context)
}
pub(super) fn source(
    source: &FromClause,
    resolver: &mut dyn VariableResolver,
    scope: &RuleBindingScope,
    context: &RuleBindingContext<'_>,
) -> Result<(), SQLError> {
    Inspector { resolver }.source(source, scope, context)
}

struct Inspector<'a> {
    resolver: &'a mut dyn VariableResolver,
}

impl Inspector<'_> {
    fn expression(
        &mut self,
        expression: &Expr,
        scope: &RuleBindingScope,
        context: &RuleBindingContext<'_>,
    ) -> Result<(), SQLError> {
        context.control().check()?;
        match expression {
            Expr::Column(_)
            | Expr::QualifiedColumn { .. }
            | Expr::Param(_)
            | Expr::InternalColumn(_)
            | Expr::QualifiedStar(_)
            | Expr::Default
            | Expr::Literal(_)
            | Expr::TypedLiteral { .. }
            | Expr::RetainedLiteral(_)
            | Expr::Star => self.reference(expression, scope)?,
            Expr::Func {
                args,
                order_by,
                filter,
                ..
            } => {
                if let Some((qualifier, column)) =
                    namespace::event_field_reference(expression, scope)
                {
                    self.resolver.rewrite_qualified(qualifier, column)?;
                } else {
                    self.expressions(args, scope, context)?;
                    self.orders(order_by, scope, context)?;
                    self.optional(filter.as_deref(), scope, context)?;
                }
            }
            Expr::Array(items) | Expr::And(items) | Expr::Or(items) => {
                self.expressions(items, scope, context)?;
            }
            Expr::Row(items) | Expr::CompositeRow { items, .. } => {
                for item in items {
                    self.expanding(item, scope, context)?;
                }
            }
            Expr::Binary { lhs, rhs, .. } => {
                self.expression(lhs, scope, context)?;
                self.expression(rhs, scope, context)?;
            }
            Expr::UnaryMinus(expr)
            | Expr::Not(expr)
            | Expr::IsNull { expr, .. }
            | Expr::Cast { expr, .. } => self.expression(expr, scope, context)?,
            Expr::Between { expr, low, high } => {
                self.expression(expr, scope, context)?;
                self.expression(low, scope, context)?;
                self.expression(high, scope, context)?;
            }
            Expr::InList { expr, list, .. } => {
                self.expression(expr, scope, context)?;
                self.expressions(list, scope, context)?;
            }
            Expr::WindowCall {
                args, spec, filter, ..
            } => {
                self.expressions(args, scope, context)?;
                self.optional(filter.as_deref(), scope, context)?;
                self.window(spec, scope, context)?;
            }
            Expr::Case {
                base,
                when,
                else_branch,
            } => {
                self.optional(base.as_deref(), scope, context)?;
                for (condition, result) in when {
                    self.expression(condition, scope, context)?;
                    self.expression(result, scope, context)?;
                }
                self.optional(else_branch.as_deref(), scope, context)?;
            }
            Expr::ScalarSubquery(query) | Expr::Exists { body: query, .. } => {
                self.select(query, scope, context)?;
            }
            Expr::InSubquery { expr, body, .. } => {
                self.expression(expr, scope, context)?;
                self.select(body, scope, context)?;
            }
        }
        Ok(())
    }

    fn reference(&mut self, expression: &Expr, scope: &RuleBindingScope) -> Result<(), SQLError> {
        match expression {
            Expr::Column(name) => {
                if !scope.qualifier_is_shadowed(name) && !scope.column_is_shadowed(name) {
                    self.resolver.rewrite_name(name)?;
                }
            }
            Expr::QualifiedColumn { qualifier, column } => {
                if !scope.qualifier_is_shadowed(qualifier) {
                    self.resolver.rewrite_qualified(qualifier, column)?;
                }
            }
            Expr::Param(index) => {
                self.resolver.rewrite_param(*index)?;
            }
            Expr::InternalColumn(column) => {
                self.resolver.rewrite_internal(*column)?;
            }
            Expr::QualifiedStar(qualifier) => {
                if !scope.qualifier_is_shadowed(qualifier) {
                    self.resolver.rewrite_qualified_whole_row(qualifier)?;
                }
            }
            Expr::Default
            | Expr::Literal(_)
            | Expr::TypedLiteral { .. }
            | Expr::RetainedLiteral(_)
            | Expr::Star => {}
            _ => unreachable!("rule reference reader received a compound expression"),
        }
        Ok(())
    }

    fn expanding(
        &mut self,
        expression: &Expr,
        scope: &RuleBindingScope,
        context: &RuleBindingContext<'_>,
    ) -> Result<(), SQLError> {
        if let Expr::QualifiedStar(qualifier) = expression {
            if !scope.qualifier_is_shadowed(qualifier)
                && self.resolver.rewrite_qualified_star(qualifier)?.is_some()
            {
                return Ok(());
            }
        }
        self.expression(expression, scope, context)
    }

    fn expressions(
        &mut self,
        items: &[Expr],
        scope: &RuleBindingScope,
        context: &RuleBindingContext<'_>,
    ) -> Result<(), SQLError> {
        for item in items {
            self.expression(item, scope, context)?;
        }
        Ok(())
    }
    fn optional(
        &mut self,
        item: Option<&Expr>,
        scope: &RuleBindingScope,
        context: &RuleBindingContext<'_>,
    ) -> Result<(), SQLError> {
        if let Some(item) = item {
            self.expression(item, scope, context)?;
        }
        Ok(())
    }
    fn orders(
        &mut self,
        items: &[OrderBy],
        scope: &RuleBindingScope,
        context: &RuleBindingContext<'_>,
    ) -> Result<(), SQLError> {
        for item in items {
            self.expression(&item.expr, scope, context)?;
        }
        Ok(())
    }
    fn projections(
        &mut self,
        items: &[Projection],
        scope: &RuleBindingScope,
        context: &RuleBindingContext<'_>,
    ) -> Result<(), SQLError> {
        for item in items {
            self.expanding(&item.expr, scope, context)?;
        }
        Ok(())
    }
    fn window(
        &mut self,
        window: &crate::ast::WindowSpec,
        scope: &RuleBindingScope,
        context: &RuleBindingContext<'_>,
    ) -> Result<(), SQLError> {
        self.expressions(&window.partition_by, scope, context)?;
        self.orders(&window.order_by, scope, context)?;
        if let Some(frame) = &window.frame {
            for bound in [&frame.start, &frame.end] {
                if let FrameBound::Preceding(expr) | FrameBound::Following(expr) = bound {
                    self.expression(expr, scope, context)?;
                }
            }
        }
        Ok(())
    }

    fn select(
        &mut self,
        select: &SelectStmt,
        inherited: &RuleBindingScope,
        context: &RuleBindingContext<'_>,
    ) -> Result<(), SQLError> {
        context.control().check()?;
        let local = context.with_ctes(&select.with)?;
        let mut scope = inherited.copy(&context.control())?;
        if let Some(source) = &select.from {
            collect_visible_scope(source, &local, &mut scope)?;
        }
        for window in &select.windows {
            self.window(&window.spec, &scope, &local)?;
        }
        self.projections(&select.projections, &scope, &local)?;
        for row in &select.values {
            for expression in row {
                self.expanding(expression, &scope, &local)?;
            }
        }
        if let Some(source) = &select.from {
            self.source(source, inherited, &local)?;
        }
        self.optional(select.r#where.as_ref(), &scope, &local)?;
        self.expressions(&select.group_by, &scope, &local)?;
        for set in &select.grouping_sets {
            self.expressions(set, &scope, &local)?;
        }
        self.optional(select.having.as_ref(), &scope, &local)?;
        self.orders(&select.order_by, &scope, &local)?;
        self.optional(select.limit.as_ref(), &scope, &local)?;
        self.optional(select.offset.as_ref(), &scope, &local)?;
        self.ctes(&select.with, inherited, context)?;
        if let Some(operation) = &select.set_op {
            if let Some(left) = &operation.left {
                self.select(left, inherited, &local)?;
            }
            self.select(&operation.right, inherited, &local)?;
            self.orders(&operation.combined_order_by, inherited, &local)?;
            self.optional(operation.combined_limit.as_ref(), inherited, &local)?;
            self.optional(operation.combined_offset.as_ref(), inherited, &local)?;
        }
        self.expressions(&select.distinct_on, &scope, &local)
    }

    fn source(
        &mut self,
        source: &FromClause,
        inherited: &RuleBindingScope,
        context: &RuleBindingContext<'_>,
    ) -> Result<(), SQLError> {
        context.control().check()?;
        match source {
            FromClause::RetainedRows(_) | FromClause::Table { .. } => {}
            FromClause::Join {
                left,
                right,
                on,
                lateral,
                ..
            } => {
                self.source(left, inherited, context)?;
                let mut right_scope = inherited.copy(&context.control())?;
                if *lateral {
                    collect_visible_scope(left, context, &mut right_scope)?;
                }
                self.source(right, &right_scope, context)?;
                let mut on_scope = inherited.copy(&context.control())?;
                collect_visible_scope(left, context, &mut on_scope)?;
                collect_visible_scope(right, context, &mut on_scope)?;
                self.optional(on.as_ref(), &on_scope, context)?;
            }
            FromClause::Values { rows, .. } => {
                for row in rows {
                    self.expressions(row, inherited, context)?;
                }
            }
            FromClause::Function { args, .. } => self.expressions(args, inherited, context)?,
            FromClause::FunctionGroup { functions, .. } => {
                for function in functions {
                    self.expressions(&function.args, inherited, context)?;
                }
            }
            FromClause::Subquery { body, .. } => self.select(body, inherited, context)?,
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
