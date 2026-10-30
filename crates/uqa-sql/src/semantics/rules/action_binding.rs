//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Query-scope-aware binding for rewrite-rule OLD/NEW pseudo-relations.

use std::collections::BTreeSet;

use crate::ast::{Expr, FrameBound, OrderBy, Projection, RuleEvent, Statement, UpdateStmt};
use crate::plan::copy::{CollectSyntax, SyntaxProduction};
use crate::plpgsql::RewriteReference;
use crate::plpgsql::{bind_statement, ResolvedVariable, VariableResolver};
use crate::SQLError;
use uqa_core::memory::{Produced, ProductionControl};

mod owned;
pub(super) use owned::returning_mut;
pub use owned::BoundRuleStatement;
mod query;
use query::{bind_ctes, bind_from, bind_select_with_scope};
mod insert;
mod inspection;
use insert::bind_insert;
mod names;
pub use names::ActionColumns;
mod namespace;
mod references;
mod returning;
mod row_expansion;
mod scope;
#[cfg(test)]
mod tests;
pub use references::{
    first_rule_row_reference_in_expr, first_rule_row_reference_in_select,
    first_rule_row_reference_in_statement, relation_rule_row_columns_with_control,
    rule_action_has_set_operation, rule_action_input_columns_with_control,
    rule_action_references_row_with_control, rule_condition_plan_references_whole_row,
    rule_condition_plan_row_columns, rule_expr_references_row, rule_expr_references_whole_row,
    rule_expr_row_columns, rule_new_row_columns, rule_new_row_columns_with_control,
    rule_statement_references_row, rule_statement_references_whole_row, rule_statement_row_columns,
};
pub use returning::expand_rule_action_returning_stars;
pub use row_expansion::expand_rule_action_row_stars;
use scope::{collect_visible_scope, cte_output_columns, RuleBindingScope};

pub fn action_target_qualifier_referenced(
    catalog: &dyn RuleSourceCatalog,
    action: &Statement,
    qualifier: &str,
) -> bool {
    namespace::action_target_qualifier_referenced(catalog, action, qualifier)
}

mod context;
use context::RuleBindingContext;
pub use context::{
    rule_action_target_columns, rule_action_target_columns_with_control,
    rule_action_target_row_type, RuleSourceCatalog,
};

/// Bind an action's event-row OLD/NEW references while preserving the action
/// row-image aliases visible to its DML RETURNING clause.
pub fn bind_rule_action(
    catalog: &dyn RuleSourceCatalog,
    action: &Statement,
    action_columns: &BTreeSet<String>,
    resolver: &mut dyn VariableResolver,
) -> Result<Statement, SQLError> {
    Ok(bind_rule_action_with_control(
        catalog,
        action,
        action_columns,
        resolver,
        &ProductionControl::uncontrolled(),
    )?
    .into_parts()
    .0)
}

/// Construct the bound immutable action under its original request allowance. Scratch scopes retire before the completed syntax is returned.
pub fn bind_rule_action_with_control(
    catalog: &dyn RuleSourceCatalog,
    action: &Statement,
    action_columns: &dyn ActionColumns,
    resolver: &mut dyn VariableResolver,
    control: &ProductionControl<'_>,
) -> Result<Produced<Statement>, SQLError> {
    BoundRuleStatement::bind(catalog, action, action_columns, resolver, control)?.into_produced()
}

struct RuleActionBodyResolver<'a, 'resolver> {
    action_columns: &'a dyn names::ActionColumns,
    action_event: RuleEvent,
    event_resolver: &'resolver mut dyn VariableResolver,
}

impl RuleActionBodyResolver<'_, '_> {
    fn is_action_column(&self, name: &str) -> bool {
        matches!(self.action_event, RuleEvent::Update | RuleEvent::Delete)
            && self.action_columns.contains_folded(name)
    }
}

impl VariableResolver for RuleActionBodyResolver<'_, '_> {
    fn rewrite_with_control(
        &mut self,
        reference: RewriteReference<'_>,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<Expr>>, SQLError> {
        control.check()?;
        if matches!(reference, RewriteReference::Name(name) if self.is_action_column(name)) {
            Ok(None)
        } else {
            self.event_resolver.rewrite_with_control(reference, control)
        }
    }

    fn expand_with_control(
        &mut self,
        qualifier: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<Vec<Expr>>>, SQLError> {
        self.event_resolver.expand_with_control(qualifier, control)
    }

    fn resolve_name(&mut self, name: &str) -> Result<Option<ResolvedVariable>, SQLError> {
        if self.is_action_column(name) {
            Ok(None)
        } else {
            self.event_resolver.resolve_name(name)
        }
    }

    fn resolve_qualified(
        &mut self,
        qualifier: &str,
        column: &str,
    ) -> Result<Option<ResolvedVariable>, SQLError> {
        self.event_resolver.resolve_qualified(qualifier, column)
    }

    fn resolve_param(&mut self, index: usize) -> Result<Option<ResolvedVariable>, SQLError> {
        self.event_resolver.resolve_param(index)
    }

    fn rewrite_name(&mut self, name: &str) -> Result<Option<Expr>, SQLError> {
        if self.is_action_column(name) {
            Ok(None)
        } else {
            self.event_resolver.rewrite_name(name)
        }
    }

    fn rewrite_qualified(
        &mut self,
        qualifier: &str,
        column: &str,
    ) -> Result<Option<Expr>, SQLError> {
        self.event_resolver.rewrite_qualified(qualifier, column)
    }

    fn rewrite_qualified_star(&mut self, qualifier: &str) -> Result<Option<Vec<Expr>>, SQLError> {
        self.event_resolver.rewrite_qualified_star(qualifier)
    }

    fn rewrite_qualified_whole_row(&mut self, qualifier: &str) -> Result<Option<Expr>, SQLError> {
        self.event_resolver.rewrite_qualified_whole_row(qualifier)
    }

    fn rewrite_param(&mut self, index: usize) -> Result<Option<Expr>, SQLError> {
        self.event_resolver.rewrite_param(index)
    }

    fn rewrite_internal(
        &mut self,
        column: crate::ast::InternalColumnRef,
    ) -> Result<Option<Expr>, SQLError> {
        self.event_resolver.rewrite_internal(column)
    }
}

/// Bind one expression while masking relation qualifiers supplied by the SQL scope around it. Nested query scopes add their own visible aliases.
pub fn bind_rule_expr_scoped(
    expr: &Expr,
    resolver: &mut dyn VariableResolver,
    shadowed: &BTreeSet<String>,
) -> Result<Expr, SQLError> {
    Ok(bind_rule_expr_scoped_with_control(
        expr,
        resolver,
        shadowed,
        &ProductionControl::uncontrolled(),
    )?
    .into_parts()
    .0)
}

pub fn bind_rule_expr_scoped_with_control(
    expr: &Expr,
    resolver: &mut dyn VariableResolver,
    shadowed: &BTreeSet<String>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Expr>, SQLError> {
    control.check()?;
    let production = SyntaxProduction::new(*control);
    let value = bind_rule_expr_with_scope(
        expr,
        resolver,
        &RuleBindingScope::from_qualifiers_with_control(shadowed, control)?,
        &RuleBindingContext::from_control(control),
        &production,
    )?;
    production.finish(value)
}

fn rewrite_reference(
    expr: &Expr,
    reference: RewriteReference<'_>,
    resolver: &mut dyn VariableResolver,
    production: &SyntaxProduction<'_>,
) -> Result<Expr, SQLError> {
    match resolver.rewrite_with_control(reference, &production.control())? {
        Some(value) => Ok(production.take(value)),
        None => production.copy(expr),
    }
}

fn bind_rule_expr_with_scope(
    expr: &Expr,
    resolver: &mut dyn VariableResolver,
    scope: &RuleBindingScope,
    context: &RuleBindingContext<'_>,
    production: &SyntaxProduction<'_>,
) -> Result<Expr, SQLError> {
    production.check()?;
    Ok(match expr {
        Expr::Column(_)
        | Expr::QualifiedColumn { .. }
        | Expr::Param(_)
        | Expr::InternalColumn(_)
        | Expr::QualifiedStar(_) => bind_reference_expression(expr, resolver, scope, production)?,
        Expr::Default
        | Expr::Literal(_)
        | Expr::TypedLiteral { .. }
        | Expr::RetainedLiteral(_)
        | Expr::Star => production.copy(expr)?,
        Expr::Func { .. } => {
            bind_rule_function_expression(expr, resolver, scope, context, production)?
        }
        Expr::Array(items) => Expr::Array(bind_exprs(items, resolver, scope, context, production)?),
        Expr::Row(items) => Expr::Row(bind_expanding_exprs(
            items, resolver, scope, context, production,
        )?),
        Expr::CompositeRow { items, binding } => Expr::CompositeRow {
            items: bind_expanding_exprs(items, resolver, scope, context, production)?,
            binding: production.copy(binding)?,
        },
        Expr::Binary { op, lhs, rhs } => Expr::Binary {
            op: *op,
            lhs: production.boxed(bind_rule_expr_with_scope(
                lhs, resolver, scope, context, production,
            )?)?,
            rhs: production.boxed(bind_rule_expr_with_scope(
                rhs, resolver, scope, context, production,
            )?)?,
        },
        Expr::UnaryMinus(inner) => Expr::UnaryMinus(production.boxed(
            bind_rule_expr_with_scope(inner, resolver, scope, context, production)?,
        )?),
        Expr::Not(inner) => Expr::Not(production.boxed(bind_rule_expr_with_scope(
            inner, resolver, scope, context, production,
        )?)?),
        Expr::And(items) => Expr::And(bind_exprs(items, resolver, scope, context, production)?),
        Expr::Or(items) => Expr::Or(bind_exprs(items, resolver, scope, context, production)?),
        Expr::IsNull { expr, negated } => Expr::IsNull {
            expr: production.boxed(bind_rule_expr_with_scope(
                expr, resolver, scope, context, production,
            )?)?,
            negated: *negated,
        },
        Expr::Between { expr, low, high } => Expr::Between {
            expr: production.boxed(bind_rule_expr_with_scope(
                expr, resolver, scope, context, production,
            )?)?,
            low: production.boxed(bind_rule_expr_with_scope(
                low, resolver, scope, context, production,
            )?)?,
            high: production.boxed(bind_rule_expr_with_scope(
                high, resolver, scope, context, production,
            )?)?,
        },
        Expr::InList {
            expr,
            list,
            negated,
        } => Expr::InList {
            expr: production.boxed(bind_rule_expr_with_scope(
                expr, resolver, scope, context, production,
            )?)?,
            list: bind_exprs(list, resolver, scope, context, production)?,
            negated: *negated,
        },
        Expr::WindowCall { .. } => {
            bind_rule_window_expression(expr, resolver, scope, context, production)?
        }
        Expr::Case { .. } => bind_rule_case_expression(expr, resolver, scope, context, production)?,
        Expr::Cast { expr, ty, implicit } => Expr::Cast {
            implicit: *implicit,
            expr: production.boxed(bind_rule_expr_with_scope(
                expr, resolver, scope, context, production,
            )?)?,
            ty: production.copy(ty)?,
        },
        Expr::ScalarSubquery(_) | Expr::Exists { .. } | Expr::InSubquery { .. } => {
            bind_query_expression(expr, resolver, scope, context, production)?
        }
    })
}

fn bind_rule_function_expression(
    expr: &Expr,
    resolver: &mut dyn VariableResolver,
    scope: &RuleBindingScope,
    context: &RuleBindingContext<'_>,
    production: &SyntaxProduction<'_>,
) -> Result<Expr, SQLError> {
    if let Some((qualifier, column)) = namespace::event_field_reference(expr, scope) {
        return match resolver.rewrite_with_control(
            RewriteReference::Qualified { qualifier, column },
            &production.control(),
        )? {
            Some(value) => Ok(production.take(value)),
            None => Ok(Expr::QualifiedColumn {
                qualifier: production.text(qualifier)?,
                column: production.text(column)?.into(),
            }),
        };
    }
    let Expr::Func {
        order_syntax,
        name,
        binding,
        args,
        distinct,
        order_by,
        filter,
    } = expr
    else {
        unreachable!("function binder received a non-function expression")
    };
    Ok(Expr::Func {
        order_syntax: *order_syntax,
        name: production.copy(name)?,
        binding: production.copy(binding)?,
        args: bind_exprs(args, resolver, scope, context, production)?,
        distinct: *distinct,
        order_by: bind_orders(order_by, resolver, scope, context, production)?,
        filter: filter
            .as_deref()
            .map(|expr| {
                bind_rule_expr_with_scope(expr, resolver, scope, context, production)
                    .and_then(|value| production.boxed(value))
            })
            .transpose()?,
    })
}

fn bind_window_spec(
    spec: &crate::ast::WindowSpec,
    resolver: &mut dyn VariableResolver,
    scope: &RuleBindingScope,
    context: &RuleBindingContext<'_>,
    production: &SyntaxProduction<'_>,
) -> Result<crate::ast::WindowSpec, SQLError> {
    Ok(crate::ast::WindowSpec {
        definition: spec.definition,
        raw_definition: None,
        reference: production.copy(&spec.reference)?,
        partition_by: bind_exprs(&spec.partition_by, resolver, scope, context, production)?,
        order_by: bind_orders(&spec.order_by, resolver, scope, context, production)?,
        frame: spec
            .frame
            .as_ref()
            .map(|frame| -> Result<crate::ast::WindowFrame, SQLError> {
                Ok(crate::ast::WindowFrame {
                    mode: frame.mode,
                    start: bind_frame_bound(&frame.start, resolver, scope, context, production)?,
                    end: bind_frame_bound(&frame.end, resolver, scope, context, production)?,
                    between: frame.between,
                    exclusion: frame.exclusion,
                })
            })
            .transpose()?,
    })
}

fn bind_rule_window_expression(
    expr: &Expr,
    resolver: &mut dyn VariableResolver,
    scope: &RuleBindingScope,
    context: &RuleBindingContext<'_>,
    production: &SyntaxProduction<'_>,
) -> Result<Expr, SQLError> {
    let Expr::WindowCall {
        name,
        args,
        spec,
        filter,
        modifiers,
    } = expr
    else {
        unreachable!("window binder received a non-window expression")
    };
    Ok(Expr::WindowCall {
        modifiers: *modifiers,
        name: production.copy(name)?,
        args: bind_exprs(args, resolver, scope, context, production)?,
        filter: filter
            .as_deref()
            .map(|filter| {
                bind_rule_expr_with_scope(filter, resolver, scope, context, production)
                    .and_then(|value| production.boxed(value))
            })
            .transpose()?,
        spec: production.boxed(bind_window_spec(
            spec, resolver, scope, context, production,
        )?)?,
    })
}

fn bind_rule_case_expression(
    expr: &Expr,
    resolver: &mut dyn VariableResolver,
    scope: &RuleBindingScope,
    context: &RuleBindingContext<'_>,
    production: &SyntaxProduction<'_>,
) -> Result<Expr, SQLError> {
    let Expr::Case {
        base,
        when,
        else_branch,
    } = expr
    else {
        unreachable!("CASE binder received a non-CASE expression")
    };
    Ok(Expr::Case {
        base: base
            .as_deref()
            .map(|expr| {
                bind_rule_expr_with_scope(expr, resolver, scope, context, production)
                    .and_then(|value| production.boxed(value))
            })
            .transpose()?,
        when: when
            .iter()
            .map(|(condition, result)| {
                Ok((
                    bind_rule_expr_with_scope(condition, resolver, scope, context, production)?,
                    bind_rule_expr_with_scope(result, resolver, scope, context, production)?,
                ))
            })
            .collect_syntax(production)?,
        else_branch: else_branch
            .as_deref()
            .map(|expr| {
                bind_rule_expr_with_scope(expr, resolver, scope, context, production)
                    .and_then(|value| production.boxed(value))
            })
            .transpose()?,
    })
}

fn bind_rule_statement_body(
    statement: &Statement,
    resolver: &mut dyn VariableResolver,
    inherited: &RuleBindingScope,
    context: &RuleBindingContext<'_>,
    production: &SyntaxProduction<'_>,
) -> Result<Statement, SQLError> {
    Ok(match statement {
        Statement::Select(select) => Statement::Select(production.boxed(
            bind_select_with_scope(select, resolver, inherited, context, production)?,
        )?),
        Statement::Insert(insert) => Statement::Insert(bind_insert(
            insert, resolver, inherited, context, production,
        )?),
        Statement::Update(update) => Statement::Update(bind_update(
            update, resolver, inherited, context, production,
        )?),
        Statement::Delete(delete) => {
            let with = bind_ctes(&delete.with, resolver, inherited, context, production)?;
            let context = context.with_ctes(&delete.with)?;
            let mut target_scope = inherited.copy(&context.control())?;
            target_scope.insert_qualifier(&delete.target_qualifier, &context.control())?;
            let using = delete
                .using
                .as_ref()
                .map(|source| bind_from(source, resolver, &target_scope, &context, production))
                .transpose()?;
            let mut expression_scope = target_scope;
            if let Some(source) = &delete.using {
                collect_visible_scope(source, &context, &mut expression_scope)?;
            }
            let predicate = bind_optional_expr(
                delete.r#where.as_ref(),
                resolver,
                &expression_scope,
                &context,
                production,
            )?;
            Statement::Delete(crate::ast::DeleteStmt {
                table: production.copy(&delete.table)?,
                target_relation_bound: delete.target_relation_bound,
                target_qualifier: production.copy(&delete.target_qualifier)?,
                target_alias: production.copy(&delete.target_alias)?,
                include_descendants: delete.include_descendants,
                with,
                using,
                r#where: predicate,
                returning: Vec::new(),
                returning_aliases: production.copy(&delete.returning_aliases)?,
            })
        }
        Statement::Notify { .. } => production.copy(statement)?,
        _ if production.control().budget().is_none() => return bind_statement(statement, resolver),
        _ => {
            return Err(SQLError::Internal(
                "stored rule action has an invalid statement kind".into(),
            ))
        }
    })
}

fn bind_assignment_targets(
    targets: &crate::ast::AssignmentTargets,
    resolver: &mut dyn VariableResolver,
    scope: &RuleBindingScope,
    context: &RuleBindingContext<'_>,
    production: &SyntaxProduction<'_>,
) -> Result<crate::ast::AssignmentTargets, SQLError> {
    use crate::ast::{AssignmentTargets, MultipleAssignmentTargets};
    Ok(match targets {
        AssignmentTargets::Single(target) => AssignmentTargets::Single(bind_assignment_target(
            target, resolver, scope, context, production,
        )?),
        AssignmentTargets::Multiple(group) => {
            AssignmentTargets::Multiple(MultipleAssignmentTargets {
                targets: group
                    .targets
                    .iter()
                    .map(|target| {
                        bind_assignment_target(target, resolver, scope, context, production)
                    })
                    .collect_syntax(production)?,
                source_positions: production.copy(&group.source_positions)?,
                source_width: group.source_width,
            })
        }
    })
}

fn bind_assignment_target(
    target: &crate::ast::AssignmentTarget,
    resolver: &mut dyn VariableResolver,
    scope: &RuleBindingScope,
    context: &RuleBindingContext<'_>,
    production: &SyntaxProduction<'_>,
) -> Result<crate::ast::AssignmentTarget, SQLError> {
    use crate::ast::{AssignmentStep, AssignmentTarget};
    Ok(AssignmentTarget {
        column: production.copy(&target.column)?,
        indirection: target
            .indirection
            .iter()
            .map(|step| {
                Ok(match step {
                    AssignmentStep::Field(name) => AssignmentStep::Field(production.copy(name)?),
                    AssignmentStep::Index(expr) => AssignmentStep::Index(production.boxed(
                        bind_rule_expr_with_scope(expr, resolver, scope, context, production)?,
                    )?),
                    AssignmentStep::Slice { lower, upper } => AssignmentStep::Slice {
                        lower: lower
                            .as_deref()
                            .map(|expr| {
                                production.boxed(bind_rule_expr_with_scope(
                                    expr, resolver, scope, context, production,
                                )?)
                            })
                            .transpose()?,
                        upper: upper
                            .as_deref()
                            .map(|expr| {
                                production.boxed(bind_rule_expr_with_scope(
                                    expr, resolver, scope, context, production,
                                )?)
                            })
                            .transpose()?,
                    },
                })
            })
            .collect_syntax(production)?,
    })
}

fn bind_update(
    update: &UpdateStmt,
    resolver: &mut dyn VariableResolver,
    inherited: &RuleBindingScope,
    context: &RuleBindingContext<'_>,
    production: &SyntaxProduction<'_>,
) -> Result<UpdateStmt, SQLError> {
    let with = bind_ctes(&update.with, resolver, inherited, context, production)?;
    let context = context.with_ctes(&update.with)?;
    let mut target_scope = inherited.copy(&context.control())?;
    target_scope.insert_qualifier(&update.target_qualifier, &context.control())?;
    let from = update
        .from
        .as_ref()
        .map(|source| bind_from(source, resolver, &target_scope, &context, production))
        .transpose()?;
    let mut expression_scope = target_scope;
    if let Some(source) = &update.from {
        collect_visible_scope(source, &context, &mut expression_scope)?;
    }
    let assignments = update
        .assignments
        .iter()
        .map(|(column, expr)| {
            Ok((
                bind_assignment_targets(column, resolver, &expression_scope, &context, production)?,
                bind_rule_expr_with_scope(expr, resolver, &expression_scope, &context, production)?,
            ))
        })
        .collect_syntax(production)?;
    let predicate = bind_optional_expr(
        update.r#where.as_ref(),
        resolver,
        &expression_scope,
        &context,
        production,
    )?;
    Ok(UpdateStmt {
        table: production.copy(&update.table)?,
        target_relation_bound: update.target_relation_bound,
        target_qualifier: production.copy(&update.target_qualifier)?,
        target_alias: production.copy(&update.target_alias)?,
        include_descendants: update.include_descendants,
        with,
        from,
        assignments,
        r#where: predicate,
        returning: Vec::new(),
        returning_aliases: production.copy(&update.returning_aliases)?,
    })
}

fn bind_frame_bound(
    bound: &FrameBound,
    resolver: &mut dyn VariableResolver,
    scope: &RuleBindingScope,
    context: &RuleBindingContext<'_>,
    production: &SyntaxProduction<'_>,
) -> Result<FrameBound, SQLError> {
    Ok(match bound {
        FrameBound::Preceding(expr) => FrameBound::Preceding(production.boxed(
            bind_rule_expr_with_scope(expr, resolver, scope, context, production)?,
        )?),
        FrameBound::Following(expr) => FrameBound::Following(production.boxed(
            bind_rule_expr_with_scope(expr, resolver, scope, context, production)?,
        )?),
        _ => production.copy(bound)?,
    })
}

fn bind_projections(
    projections: &[Projection],
    resolver: &mut dyn VariableResolver,
    scope: &RuleBindingScope,
    context: &RuleBindingContext<'_>,
    production: &SyntaxProduction<'_>,
) -> Result<Vec<Projection>, SQLError> {
    let mut bound = production.vector(projections.len())?;
    for projection in projections {
        if let Some(expressions) =
            expand_qualified_star(&projection.expr, resolver, scope, production)?
        {
            for expr in production.elements(expressions) {
                bound.push(Projection {
                    expr,
                    alias: production.copy(&projection.alias)?,
                })?;
            }
        } else {
            bound.push(Projection {
                expr: bind_rule_expr_with_scope(
                    &projection.expr,
                    resolver,
                    scope,
                    context,
                    production,
                )?,
                alias: production.copy(&projection.alias)?,
            })?;
        }
    }
    bound.finish()
}

fn bind_expanding_exprs(
    expressions: &[Expr],
    resolver: &mut dyn VariableResolver,
    scope: &RuleBindingScope,
    context: &RuleBindingContext<'_>,
    production: &SyntaxProduction<'_>,
) -> Result<Vec<Expr>, SQLError> {
    let mut bound = production.vector(expressions.len())?;
    for expression in expressions {
        if let Some(expressions) = expand_qualified_star(expression, resolver, scope, production)? {
            for expression in production.elements(expressions) {
                bound.push(expression)?;
            }
        } else {
            bound.push(bind_rule_expr_with_scope(
                expression, resolver, scope, context, production,
            )?)?;
        }
    }
    bound.finish()
}

fn expand_qualified_star(
    expression: &Expr,
    resolver: &mut dyn VariableResolver,
    scope: &RuleBindingScope,
    production: &SyntaxProduction<'_>,
) -> Result<Option<Produced<Vec<Expr>>>, SQLError> {
    let Expr::QualifiedStar(qualifier) = expression else {
        return Ok(None);
    };
    if scope.qualifier_is_shadowed(qualifier) {
        return Ok(None);
    }
    resolver.expand_with_control(qualifier, &production.control())
}

fn bind_exprs(
    expressions: &[Expr],
    resolver: &mut dyn VariableResolver,
    scope: &RuleBindingScope,
    context: &RuleBindingContext<'_>,
    production: &SyntaxProduction<'_>,
) -> Result<Vec<Expr>, SQLError> {
    expressions
        .iter()
        .map(|expr| bind_rule_expr_with_scope(expr, resolver, scope, context, production))
        .collect_syntax(production)
}

fn bind_optional_expr(
    expression: Option<&Expr>,
    resolver: &mut dyn VariableResolver,
    scope: &RuleBindingScope,
    context: &RuleBindingContext<'_>,
    production: &SyntaxProduction<'_>,
) -> Result<Option<Expr>, SQLError> {
    expression
        .map(|expr| bind_rule_expr_with_scope(expr, resolver, scope, context, production))
        .transpose()
}

fn bind_orders(
    orders: &[OrderBy],
    resolver: &mut dyn VariableResolver,
    scope: &RuleBindingScope,
    context: &RuleBindingContext<'_>,
    production: &SyntaxProduction<'_>,
) -> Result<Vec<OrderBy>, SQLError> {
    orders
        .iter()
        .map(|order| {
            Ok(OrderBy {
                expr: bind_rule_expr_with_scope(&order.expr, resolver, scope, context, production)?,
                descending: order.descending,
                nulls: order.nulls,
            })
        })
        .collect_syntax(production)
}

pub fn invalid_rule_relation_reference(qualifier: &str) -> SQLError {
    SQLError::Diagnostic {
        sqlstate: "42P01".into(),
        message: format!("invalid reference to FROM-clause entry for table \"{qualifier}\""),
        detail: Some(format!(
            "There is an entry for table \"{qualifier}\", but it cannot be referenced from this part of the query."
        )),
        hint: None,
    }
}

fn bind_reference_expression(
    expr: &Expr,
    resolver: &mut dyn VariableResolver,
    scope: &RuleBindingScope,
    production: &SyntaxProduction<'_>,
) -> Result<Expr, SQLError> {
    Ok(match expr {
        Expr::Column(name) => {
            if scope.qualifier_is_shadowed(name) || scope.column_is_shadowed(name) {
                production.copy(expr)?
            } else {
                rewrite_reference(expr, RewriteReference::Name(name), resolver, production)?
            }
        }
        Expr::QualifiedColumn { qualifier, column } => {
            if scope.qualifier_is_shadowed(qualifier) {
                production.copy(expr)?
            } else {
                rewrite_reference(
                    expr,
                    RewriteReference::Qualified { qualifier, column },
                    resolver,
                    production,
                )?
            }
        }
        Expr::Param(index) => rewrite_reference(
            expr,
            RewriteReference::Parameter(*index),
            resolver,
            production,
        )?,
        Expr::InternalColumn(column) => rewrite_reference(
            expr,
            RewriteReference::Internal(*column),
            resolver,
            production,
        )?,
        Expr::QualifiedStar(qualifier) => {
            if scope.qualifier_is_shadowed(qualifier) {
                production.copy(expr)?
            } else {
                rewrite_reference(
                    expr,
                    RewriteReference::WholeRow(qualifier),
                    resolver,
                    production,
                )?
            }
        }
        _ => unreachable!("reference binder received a different expression"),
    })
}

fn bind_query_expression(
    expr: &Expr,
    resolver: &mut dyn VariableResolver,
    scope: &RuleBindingScope,
    context: &RuleBindingContext<'_>,
    production: &SyntaxProduction<'_>,
) -> Result<Expr, SQLError> {
    Ok(match expr {
        Expr::ScalarSubquery(body) => Expr::ScalarSubquery(production.boxed(
            bind_select_with_scope(body, resolver, scope, context, production)?,
        )?),
        Expr::Exists { body, negated } => Expr::Exists {
            body: production.boxed(bind_select_with_scope(
                body, resolver, scope, context, production,
            )?)?,
            negated: *negated,
        },
        Expr::InSubquery {
            expr,
            body,
            negated,
        } => Expr::InSubquery {
            expr: production.boxed(bind_rule_expr_with_scope(
                expr, resolver, scope, context, production,
            )?)?,
            body: production.boxed(bind_select_with_scope(
                body, resolver, scope, context, production,
            )?)?,
            negated: *negated,
        },
        _ => unreachable!("query binder received a different expression"),
    })
}
