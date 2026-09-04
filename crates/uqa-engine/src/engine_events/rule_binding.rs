//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Query-scope-aware binding for rewrite-rule OLD/NEW pseudo-relations.

use std::collections::{BTreeMap, BTreeSet};

use uqa_execution::ScalarExpr;
use uqa_planner::ExpressionPlan;
use uqa_sql::ast::{
    Expr, FrameBound, FromClause, InsertStmt, OnConflictAction, OrderBy, Projection, RuleEvent,
    SelectStmt, Statement, UpdateStmt, CTE,
};
use uqa_sql::plpgsql::{bind_statement, ResolvedVariable, VariableResolver};
use uqa_sql::SQLError;

mod namespace;
mod returning;
mod row_expansion;
pub(crate) use returning::{
    expand_rule_action_returning_stars, rename_rule_action_returning_target_column,
    rule_action_returning_references_target_column,
};
pub(crate) use row_expansion::expand_rule_action_row_stars;

pub(super) fn action_target_qualifier_referenced(
    engine: &crate::Engine,
    action: &Statement,
    qualifier: &str,
) -> bool {
    namespace::action_target_qualifier_referenced(engine, action, qualifier)
}

#[derive(Default)]
struct RuleRowReferenceDetector {
    qualifier: Option<String>,
    whole_row: bool,
}

#[derive(Default)]
struct RuleRowColumnCollector {
    columns: BTreeSet<String>,
}

/// Names introduced by a nested query that take precedence over rule OLD/NEW rows.
#[derive(Clone, Default)]
pub(super) struct RuleBindingScope {
    qualifiers: BTreeSet<String>,
    columns: BTreeSet<String>,
}

#[derive(Clone, Default)]
struct RuleBindingContext<'a> {
    engine: Option<&'a crate::Engine>,
    ctes: BTreeMap<String, Vec<String>>,
}

#[derive(Default)]
struct RuleSourceNames {
    columns: Vec<String>,
    qualified_columns: BTreeMap<String, Vec<String>>,
}

impl<'a> RuleBindingContext<'a> {
    fn with_engine(engine: &'a crate::Engine) -> Self {
        Self {
            engine: Some(engine),
            ctes: BTreeMap::new(),
        }
    }

    fn relation_columns(&self, name: &str) -> Result<Vec<String>, SQLError> {
        if let Some(columns) = self.ctes.get(&name.to_ascii_lowercase()) {
            return Ok(columns.clone());
        }
        self.engine.map_or_else(
            || Ok(Vec::new()),
            |engine| {
                crate::sql::query_source_column_names(engine, name)?
                    .ok_or_else(|| SQLError::UnknownTable(name.to_string()))
            },
        )
    }

    fn with_ctes(&self, ctes: &[CTE]) -> Result<Self, SQLError> {
        let mut context = self.clone();
        for cte in ctes {
            let key = cte.name.to_ascii_lowercase();
            if cte.recursive {
                context.ctes.entry(key.clone()).or_default();
            }
            let mut columns = select_output_columns(&cte.query, &context)?;
            apply_positional_aliases(&mut columns, &cte.columns);
            if let Some(search) = &cte.search {
                columns.push(search.sequence_column.clone());
            }
            if let Some(cycle) = &cte.cycle {
                columns.push(cycle.mark_column.clone());
                columns.push(cycle.path_column.clone());
            }
            context.ctes.insert(key, columns);
        }
        Ok(context)
    }
}

impl RuleSourceNames {
    fn insert_qualifier(&mut self, qualifier: &str, columns: &[String]) {
        self.qualified_columns
            .insert(qualifier.to_ascii_lowercase(), columns.to_vec());
    }

    fn qualified(&self, qualifier: &str) -> Option<&[String]> {
        self.qualified_columns
            .get(&qualifier.to_ascii_lowercase())
            .map(Vec::as_slice)
    }
}

impl RuleBindingScope {
    fn from_qualifiers(qualifiers: &BTreeSet<String>) -> Self {
        Self {
            qualifiers: qualifiers
                .iter()
                .map(|qualifier| qualifier.to_ascii_lowercase())
                .collect(),
            columns: BTreeSet::new(),
        }
    }

    fn insert_qualifier(&mut self, qualifier: &str) {
        self.qualifiers.insert(qualifier.to_ascii_lowercase());
    }

    fn insert_column(&mut self, column: &str) {
        self.columns.insert(column.to_ascii_lowercase());
    }

    fn qualifier_is_shadowed(&self, qualifier: &str) -> bool {
        self.qualifiers.contains(&qualifier.to_ascii_lowercase())
    }

    fn column_is_shadowed(&self, column: &str) -> bool {
        self.columns.contains(&column.to_ascii_lowercase())
    }
}

impl VariableResolver for RuleRowColumnCollector {
    fn resolve_name(&mut self, _name: &str) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }

    fn resolve_qualified(
        &mut self,
        qualifier: &str,
        column: &str,
    ) -> Result<Option<ResolvedVariable>, SQLError> {
        if qualifier.eq_ignore_ascii_case("old") || qualifier.eq_ignore_ascii_case("new") {
            self.columns.insert(column.to_string());
        }
        Ok(None)
    }

    fn resolve_param(&mut self, _index: usize) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }
}

impl VariableResolver for RuleRowReferenceDetector {
    fn resolve_name(&mut self, name: &str) -> Result<Option<ResolvedVariable>, SQLError> {
        if name.eq_ignore_ascii_case("old") || name.eq_ignore_ascii_case("new") {
            self.observe(name, true);
        }
        Ok(None)
    }

    fn resolve_qualified(
        &mut self,
        qualifier: &str,
        _column: &str,
    ) -> Result<Option<ResolvedVariable>, SQLError> {
        self.observe(qualifier, false);
        Ok(None)
    }

    fn resolve_param(&mut self, _index: usize) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }

    fn rewrite_qualified_whole_row(&mut self, qualifier: &str) -> Result<Option<Expr>, SQLError> {
        self.observe(qualifier, true);
        Ok(None)
    }
}

impl RuleRowReferenceDetector {
    fn observe(&mut self, qualifier: &str, whole_row: bool) {
        if qualifier.eq_ignore_ascii_case("old") || qualifier.eq_ignore_ascii_case("new") {
            if self.qualifier.is_none() {
                self.qualifier = Some(qualifier.to_ascii_lowercase());
            }
            self.whole_row |= whole_row;
        }
    }
}

pub(crate) fn rule_action_has_set_operation(action: &Statement) -> bool {
    match action {
        Statement::Select(select) => select.set_op.is_some(),
        Statement::Insert(insert) => insert
            .select_source
            .as_ref()
            .is_some_and(|select| select.set_op.is_some()),
        _ => false,
    }
}

pub(crate) fn first_rule_row_reference_in_expr(
    expr: &Expr,
    shadowed: &BTreeSet<String>,
) -> Option<String> {
    let mut detector = RuleRowReferenceDetector::default();
    let _ = bind_rule_expr_scoped(expr, &mut detector, shadowed);
    detector.qualifier
}

pub(crate) fn first_rule_row_reference_in_select(
    engine: &crate::Engine,
    select: &SelectStmt,
) -> Option<String> {
    let mut detector = RuleRowReferenceDetector::default();
    let _ = bind_rule_select_scoped(engine, select, &mut detector);
    detector.qualifier
}

pub(crate) fn rule_expr_references_row(expr: &Expr) -> bool {
    first_rule_row_reference_in_expr(expr, &BTreeSet::new()).is_some()
}

pub(crate) fn rule_expr_references_whole_row(expr: &Expr) -> bool {
    let mut detector = RuleRowReferenceDetector::default();
    let _ = bind_rule_expr_scoped(expr, &mut detector, &BTreeSet::new());
    detector.whole_row
}

pub(crate) fn rule_statement_references_row(
    engine: &crate::Engine,
    statement: &Statement,
    action_columns: &BTreeSet<String>,
) -> Result<bool, SQLError> {
    let mut detector = RuleRowReferenceDetector::default();
    let _ = bind_rule_action(engine, statement, action_columns, &mut detector)?;
    Ok(detector.qualifier.is_some())
}

pub(crate) fn rule_statement_references_whole_row(
    engine: &crate::Engine,
    statement: &Statement,
    action_columns: &BTreeSet<String>,
) -> Result<bool, SQLError> {
    let mut detector = RuleRowReferenceDetector::default();
    let _ = bind_rule_action(engine, statement, action_columns, &mut detector)?;
    Ok(detector.whole_row)
}

pub(crate) fn rule_expr_row_columns(expr: &Expr) -> BTreeSet<String> {
    let mut collector = RuleRowColumnCollector::default();
    let _ = bind_rule_expr_scoped(expr, &mut collector, &BTreeSet::new());
    collector.columns
}

fn collect_condition_plan_row_column(expression: &ScalarExpr, columns: &mut BTreeSet<String>) {
    if let ScalarExpr::QualifiedColumn { qualifier, column } = expression {
        if qualifier == super::RULE_OLD_PLAN_QUALIFIER
            || qualifier == super::RULE_NEW_PLAN_QUALIFIER
        {
            columns.insert(column.clone());
        }
    }
}

fn condition_plan_expression_references_whole_row(expression: &ScalarExpr) -> bool {
    matches!(
        expression,
        ScalarExpr::Column(qualifier) | ScalarExpr::QualifiedStar(qualifier)
            if qualifier.eq_ignore_ascii_case("old")
                || qualifier.eq_ignore_ascii_case("new")
                || qualifier == super::RULE_OLD_PLAN_QUALIFIER
                || qualifier == super::RULE_NEW_PLAN_QUALIFIER
    )
}

pub(crate) fn rule_condition_plan_references_whole_row(plan: &ExpressionPlan) -> bool {
    let mut referenced = false;
    plan.scalar.visit(&mut |expression| {
        referenced |= condition_plan_expression_references_whole_row(expression);
    });
    for subquery in &plan.subqueries {
        let mut subquery = subquery.clone();
        subquery.rewrite_scalar_expressions(&mut |expression| {
            referenced |= condition_plan_expression_references_whole_row(expression);
        });
    }
    referenced
}

pub(crate) fn rule_condition_plan_row_columns(plan: &ExpressionPlan) -> BTreeSet<String> {
    let mut columns = BTreeSet::new();
    plan.scalar
        .visit(&mut |expression| collect_condition_plan_row_column(expression, &mut columns));
    for subquery in &plan.subqueries {
        let mut subquery = subquery.clone();
        subquery.rewrite_scalar_expressions(&mut |expression| {
            collect_condition_plan_row_column(expression, &mut columns);
        });
    }
    columns
}

pub(crate) fn rename_rule_condition_plan_column(plan: &mut ExpressionPlan, from: &str, to: &str) {
    let mut rewrite = |expression: &mut ScalarExpr| {
        if let ScalarExpr::QualifiedColumn { qualifier, column } = expression {
            if (qualifier == super::RULE_OLD_PLAN_QUALIFIER
                || qualifier == super::RULE_NEW_PLAN_QUALIFIER)
                && column == from
            {
                *column = to.to_string();
            }
        }
    };
    uqa_planner::rewrite_scalar_expression(&mut plan.scalar, &mut rewrite);
    for subquery in &mut plan.subqueries {
        subquery.rewrite_scalar_expressions(&mut rewrite);
    }
}

pub(crate) fn rule_statement_row_columns(
    engine: &crate::Engine,
    statement: &Statement,
    action_columns: &BTreeSet<String>,
) -> Result<BTreeSet<String>, SQLError> {
    let mut collector = RuleRowColumnCollector::default();
    let _ = bind_rule_action(engine, statement, action_columns, &mut collector)?;
    Ok(collector.columns)
}

/// Bind an action's event-row OLD/NEW references while preserving the action
/// row-image aliases visible to its DML RETURNING clause.
pub(crate) fn bind_rule_action(
    engine: &crate::Engine,
    action: &Statement,
    action_columns: &BTreeSet<String>,
    resolver: &mut dyn VariableResolver,
) -> Result<Statement, SQLError> {
    let context = RuleBindingContext::with_engine(engine);
    let (returning, action_event) = match action {
        Statement::Insert(statement) => (&statement.returning, RuleEvent::Insert),
        Statement::Update(statement) => (&statement.returning, RuleEvent::Update),
        Statement::Delete(statement) => (&statement.returning, RuleEvent::Delete),
        _ => {
            return bind_rule_statement_body(
                action,
                resolver,
                &RuleBindingScope::default(),
                &context,
            )
        }
    };
    let mut body = action.clone();
    let aliases = match &mut body {
        Statement::Insert(statement) => {
            statement.returning.clear();
            statement.returning_aliases.clone()
        }
        Statement::Update(statement) => {
            statement.returning.clear();
            statement.returning_aliases.clone()
        }
        Statement::Delete(statement) => {
            statement.returning.clear();
            statement.returning_aliases.clone()
        }
        _ => unreachable!("DML rule action changed statement kind"),
    };
    let mut bound = {
        let mut body_resolver = RuleActionBodyResolver {
            action_columns,
            action_event,
            event_resolver: resolver,
        };
        bind_rule_statement_body(
            &body,
            &mut body_resolver,
            &RuleBindingScope::default(),
            &context,
        )?
    };
    if returning.is_empty() {
        return Ok(bound);
    }
    let returning = returning::bind_rule_action_returning(
        returning,
        action_columns,
        &aliases,
        action_event,
        resolver,
        &context,
    )?;
    match &mut bound {
        Statement::Insert(statement) => statement.returning = returning,
        Statement::Update(statement) => statement.returning = returning,
        Statement::Delete(statement) => statement.returning = returning,
        _ => unreachable!("bound DML rule action changed statement kind"),
    }
    Ok(bound)
}

struct RuleActionBodyResolver<'a, 'resolver> {
    action_columns: &'a BTreeSet<String>,
    action_event: RuleEvent,
    event_resolver: &'resolver mut dyn VariableResolver,
}

impl RuleActionBodyResolver<'_, '_> {
    fn is_action_column(&self, name: &str) -> bool {
        matches!(self.action_event, RuleEvent::Update | RuleEvent::Delete)
            && self
                .action_columns
                .iter()
                .any(|column| column.eq_ignore_ascii_case(name))
    }
}

impl VariableResolver for RuleActionBodyResolver<'_, '_> {
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
        column: uqa_sql::ast::InternalColumnRef,
    ) -> Result<Option<Expr>, SQLError> {
        self.event_resolver.rewrite_internal(column)
    }
}

/// Bind one expression while masking relation qualifiers supplied by the SQL scope around it. Nested query scopes add their own visible aliases.
pub(crate) fn bind_rule_expr_scoped(
    expr: &Expr,
    resolver: &mut dyn VariableResolver,
    shadowed: &BTreeSet<String>,
) -> Result<Expr, SQLError> {
    bind_rule_expr_with_scope(
        expr,
        resolver,
        &RuleBindingScope::from_qualifiers(shadowed),
        &RuleBindingContext::default(),
    )
}

fn bind_rule_expr_with_scope(
    expr: &Expr,
    resolver: &mut dyn VariableResolver,
    scope: &RuleBindingScope,
    context: &RuleBindingContext<'_>,
) -> Result<Expr, SQLError> {
    Ok(match expr {
        Expr::Column(name) => {
            if scope.qualifier_is_shadowed(name) || scope.column_is_shadowed(name) {
                expr.clone()
            } else {
                resolver.rewrite_name(name)?.unwrap_or_else(|| expr.clone())
            }
        }
        Expr::QualifiedColumn { qualifier, column } => {
            if scope.qualifier_is_shadowed(qualifier) {
                expr.clone()
            } else {
                resolver
                    .rewrite_qualified(qualifier, column)?
                    .unwrap_or_else(|| expr.clone())
            }
        }
        Expr::Param(index) => resolver
            .rewrite_param(*index)?
            .unwrap_or_else(|| expr.clone()),
        Expr::InternalColumn(column) => resolver
            .rewrite_internal(*column)?
            .unwrap_or_else(|| expr.clone()),
        Expr::QualifiedStar(qualifier) => {
            if scope.qualifier_is_shadowed(qualifier) {
                expr.clone()
            } else {
                resolver
                    .rewrite_qualified_whole_row(qualifier)?
                    .unwrap_or_else(|| expr.clone())
            }
        }
        Expr::Default | Expr::Literal(_) | Expr::Star => expr.clone(),
        Expr::Func { .. } => bind_rule_function_expression(expr, resolver, scope, context)?,
        Expr::Array(items) => Expr::Array(bind_exprs(items, resolver, scope, context)?),
        Expr::Row(items) => Expr::Row(bind_expanding_exprs(items, resolver, scope, context)?),
        Expr::Binary { op, lhs, rhs } => Expr::Binary {
            op: *op,
            lhs: Box::new(bind_rule_expr_with_scope(lhs, resolver, scope, context)?),
            rhs: Box::new(bind_rule_expr_with_scope(rhs, resolver, scope, context)?),
        },
        Expr::UnaryMinus(inner) => Expr::UnaryMinus(Box::new(bind_rule_expr_with_scope(
            inner, resolver, scope, context,
        )?)),
        Expr::Not(inner) => Expr::Not(Box::new(bind_rule_expr_with_scope(
            inner, resolver, scope, context,
        )?)),
        Expr::And(items) => Expr::And(bind_exprs(items, resolver, scope, context)?),
        Expr::Or(items) => Expr::Or(bind_exprs(items, resolver, scope, context)?),
        Expr::IsNull { expr, negated } => Expr::IsNull {
            expr: Box::new(bind_rule_expr_with_scope(expr, resolver, scope, context)?),
            negated: *negated,
        },
        Expr::Between { expr, low, high } => Expr::Between {
            expr: Box::new(bind_rule_expr_with_scope(expr, resolver, scope, context)?),
            low: Box::new(bind_rule_expr_with_scope(low, resolver, scope, context)?),
            high: Box::new(bind_rule_expr_with_scope(high, resolver, scope, context)?),
        },
        Expr::InList {
            expr,
            list,
            negated,
        } => Expr::InList {
            expr: Box::new(bind_rule_expr_with_scope(expr, resolver, scope, context)?),
            list: bind_exprs(list, resolver, scope, context)?,
            negated: *negated,
        },
        Expr::WindowCall { .. } => bind_rule_window_expression(expr, resolver, scope, context)?,
        Expr::Case { .. } => bind_rule_case_expression(expr, resolver, scope, context)?,
        Expr::Cast { expr, ty } => Expr::Cast {
            expr: Box::new(bind_rule_expr_with_scope(expr, resolver, scope, context)?),
            ty: ty.clone(),
        },
        Expr::ScalarSubquery(body) => Expr::ScalarSubquery(Box::new(bind_select_with_scope(
            body, resolver, scope, context,
        )?)),
        Expr::Exists { body, negated } => Expr::Exists {
            body: Box::new(bind_select_with_scope(body, resolver, scope, context)?),
            negated: *negated,
        },
        Expr::InSubquery {
            expr,
            body,
            negated,
        } => Expr::InSubquery {
            expr: Box::new(bind_rule_expr_with_scope(expr, resolver, scope, context)?),
            body: Box::new(bind_select_with_scope(body, resolver, scope, context)?),
            negated: *negated,
        },
    })
}

fn bind_rule_function_expression(
    expr: &Expr,
    resolver: &mut dyn VariableResolver,
    scope: &RuleBindingScope,
    context: &RuleBindingContext<'_>,
) -> Result<Expr, SQLError> {
    let Expr::Func {
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
        name: name.clone(),
        binding: binding.clone(),
        args: bind_exprs(args, resolver, scope, context)?,
        distinct: *distinct,
        order_by: bind_orders(order_by, resolver, scope, context)?,
        filter: filter
            .as_deref()
            .map(|expr| bind_rule_expr_with_scope(expr, resolver, scope, context).map(Box::new))
            .transpose()?,
    })
}

fn bind_rule_window_expression(
    expr: &Expr,
    resolver: &mut dyn VariableResolver,
    scope: &RuleBindingScope,
    context: &RuleBindingContext<'_>,
) -> Result<Expr, SQLError> {
    let Expr::WindowCall { name, args, spec } = expr else {
        unreachable!("window binder received a non-window expression")
    };
    Ok(Expr::WindowCall {
        name: name.clone(),
        args: bind_exprs(args, resolver, scope, context)?,
        spec: uqa_sql::ast::WindowSpec {
            reference: spec.reference.clone(),
            partition_by: bind_exprs(&spec.partition_by, resolver, scope, context)?,
            order_by: bind_orders(&spec.order_by, resolver, scope, context)?,
            frame: spec
                .frame
                .as_ref()
                .map(|frame| -> Result<uqa_sql::ast::WindowFrame, SQLError> {
                    Ok(uqa_sql::ast::WindowFrame {
                        mode: frame.mode,
                        start: bind_frame_bound(&frame.start, resolver, scope, context)?,
                        end: bind_frame_bound(&frame.end, resolver, scope, context)?,
                    })
                })
                .transpose()?,
        },
    })
}

fn bind_rule_case_expression(
    expr: &Expr,
    resolver: &mut dyn VariableResolver,
    scope: &RuleBindingScope,
    context: &RuleBindingContext<'_>,
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
            .map(|expr| bind_rule_expr_with_scope(expr, resolver, scope, context).map(Box::new))
            .transpose()?,
        when: when
            .iter()
            .map(|(condition, result)| {
                Ok((
                    bind_rule_expr_with_scope(condition, resolver, scope, context)?,
                    bind_rule_expr_with_scope(result, resolver, scope, context)?,
                ))
            })
            .collect::<Result<Vec<_>, SQLError>>()?,
        else_branch: else_branch
            .as_deref()
            .map(|expr| bind_rule_expr_with_scope(expr, resolver, scope, context).map(Box::new))
            .transpose()?,
    })
}

fn bind_rule_select_scoped(
    engine: &crate::Engine,
    select: &SelectStmt,
    resolver: &mut dyn VariableResolver,
) -> Result<SelectStmt, SQLError> {
    bind_select_with_scope(
        select,
        resolver,
        &RuleBindingScope::default(),
        &RuleBindingContext::with_engine(engine),
    )
}

fn bind_rule_statement_body(
    statement: &Statement,
    resolver: &mut dyn VariableResolver,
    inherited: &RuleBindingScope,
    context: &RuleBindingContext<'_>,
) -> Result<Statement, SQLError> {
    Ok(match statement {
        Statement::Select(select) => Statement::Select(Box::new(bind_select_with_scope(
            select, resolver, inherited, context,
        )?)),
        Statement::Insert(insert) => {
            Statement::Insert(bind_insert(insert, resolver, inherited, context)?)
        }
        Statement::Update(update) => {
            Statement::Update(bind_update(update, resolver, inherited, context)?)
        }
        Statement::Delete(delete) => {
            let mut output = delete.clone();
            output.with = bind_ctes(&delete.with, resolver, inherited, context)?;
            let context = context.with_ctes(&delete.with)?;
            let mut target_scope = inherited.clone();
            target_scope.insert_qualifier(&delete.target_qualifier);
            output.using = delete
                .using
                .as_ref()
                .map(|source| bind_from(source, resolver, &target_scope, &context))
                .transpose()?;
            let mut expression_scope = target_scope;
            if let Some(source) = &delete.using {
                collect_visible_scope(source, &context, &mut expression_scope)?;
            }
            output.r#where = bind_optional_expr(
                delete.r#where.as_ref(),
                resolver,
                &expression_scope,
                &context,
            )?;
            output.returning.clear();
            Statement::Delete(output)
        }
        _ => return bind_statement(statement, resolver),
    })
}

fn bind_insert(
    insert: &InsertStmt,
    resolver: &mut dyn VariableResolver,
    inherited: &RuleBindingScope,
    context: &RuleBindingContext<'_>,
) -> Result<InsertStmt, SQLError> {
    let mut output = insert.clone();
    output.with = bind_ctes(&insert.with, resolver, inherited, context)?;
    let context = context.with_ctes(&insert.with)?;
    output.rows = insert
        .rows
        .iter()
        .map(|row| bind_expanding_exprs(row, resolver, inherited, &context))
        .collect::<Result<Vec<_>, SQLError>>()?;
    output.select_source = insert
        .select_source
        .as_deref()
        .map(|select| bind_select_with_scope(select, resolver, inherited, &context).map(Box::new))
        .transpose()?;
    output.on_conflict = insert
        .on_conflict
        .as_ref()
        .map(|conflict| -> Result<uqa_sql::ast::OnConflict, SQLError> {
            let mut conflict_scope = inherited.clone();
            conflict_scope.insert_qualifier(&insert.target_qualifier);
            Ok(uqa_sql::ast::OnConflict {
                conflict_columns: conflict.conflict_columns.clone(),
                action: match &conflict.action {
                    OnConflictAction::Nothing => OnConflictAction::Nothing,
                    OnConflictAction::Update {
                        assignments,
                        r#where,
                    } => OnConflictAction::Update {
                        assignments: assignments
                            .iter()
                            .map(|(column, expr)| {
                                Ok((
                                    column.clone(),
                                    bind_rule_expr_with_scope(
                                        expr,
                                        resolver,
                                        &conflict_scope,
                                        &context,
                                    )?,
                                ))
                            })
                            .collect::<Result<Vec<_>, SQLError>>()?,
                        r#where: bind_optional_expr(
                            r#where.as_ref(),
                            resolver,
                            &conflict_scope,
                            &context,
                        )?,
                    },
                },
            })
        })
        .transpose()?;
    output.returning.clear();
    Ok(output)
}

fn bind_update(
    update: &UpdateStmt,
    resolver: &mut dyn VariableResolver,
    inherited: &RuleBindingScope,
    context: &RuleBindingContext<'_>,
) -> Result<UpdateStmt, SQLError> {
    let mut output = update.clone();
    output.with = bind_ctes(&update.with, resolver, inherited, context)?;
    let context = context.with_ctes(&update.with)?;
    let mut target_scope = inherited.clone();
    target_scope.insert_qualifier(&update.target_qualifier);
    output.from = update
        .from
        .as_ref()
        .map(|source| bind_from(source, resolver, &target_scope, &context))
        .transpose()?;
    let mut expression_scope = target_scope;
    if let Some(source) = &update.from {
        collect_visible_scope(source, &context, &mut expression_scope)?;
    }
    output.assignments = update
        .assignments
        .iter()
        .map(|(column, expr)| {
            Ok((
                column.clone(),
                bind_rule_expr_with_scope(expr, resolver, &expression_scope, &context)?,
            ))
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    output.r#where = bind_optional_expr(
        update.r#where.as_ref(),
        resolver,
        &expression_scope,
        &context,
    )?;
    output.returning.clear();
    Ok(output)
}

fn bind_select_with_scope(
    select: &SelectStmt,
    resolver: &mut dyn VariableResolver,
    inherited: &RuleBindingScope,
    context: &RuleBindingContext<'_>,
) -> Result<SelectStmt, SQLError> {
    let local_context = context.with_ctes(&select.with)?;
    let mut scope = inherited.clone();
    if let Some(source) = &select.from {
        collect_visible_scope(source, &local_context, &mut scope)?;
    }
    Ok(SelectStmt {
        projections: bind_projections(&select.projections, resolver, &scope, &local_context)?,
        values: select
            .values
            .iter()
            .map(|row| bind_expanding_exprs(row, resolver, &scope, &local_context))
            .collect::<Result<Vec<_>, SQLError>>()?,
        from: select
            .from
            .as_ref()
            .map(|source| bind_from(source, resolver, inherited, &local_context))
            .transpose()?,
        r#where: bind_optional_expr(select.r#where.as_ref(), resolver, &scope, &local_context)?,
        group_by: bind_exprs(&select.group_by, resolver, &scope, &local_context)?,
        grouping_sets: select
            .grouping_sets
            .iter()
            .map(|set| bind_exprs(set, resolver, &scope, &local_context))
            .collect::<Result<Vec<_>, SQLError>>()?,
        group_distinct: select.group_distinct,
        having: bind_optional_expr(select.having.as_ref(), resolver, &scope, &local_context)?,
        order_by: bind_orders(&select.order_by, resolver, &scope, &local_context)?,
        limit: bind_optional_expr(select.limit.as_ref(), resolver, &scope, &local_context)?,
        with_ties: select.with_ties,
        offset: bind_optional_expr(select.offset.as_ref(), resolver, &scope, &local_context)?,
        with: bind_ctes(&select.with, resolver, inherited, context)?,
        set_op: select
            .set_op
            .as_ref()
            .map(|operation| -> Result<Box<uqa_sql::ast::SetOp>, SQLError> {
                Ok(Box::new(uqa_sql::ast::SetOp {
                    kind: operation.kind,
                    all: operation.all,
                    left: operation
                        .left
                        .as_deref()
                        .map(|left| {
                            bind_select_with_scope(left, resolver, inherited, &local_context)
                                .map(Box::new)
                        })
                        .transpose()?,
                    right: bind_select_with_scope(
                        &operation.right,
                        resolver,
                        inherited,
                        &local_context,
                    )?,
                    combined_order_by: bind_orders(
                        &operation.combined_order_by,
                        resolver,
                        inherited,
                        &local_context,
                    )?,
                    combined_limit: bind_optional_expr(
                        operation.combined_limit.as_ref(),
                        resolver,
                        inherited,
                        &local_context,
                    )?,
                    combined_with_ties: operation.combined_with_ties,
                    combined_offset: bind_optional_expr(
                        operation.combined_offset.as_ref(),
                        resolver,
                        inherited,
                        &local_context,
                    )?,
                }))
            })
            .transpose()?,
        distinct: select.distinct,
        distinct_on: bind_exprs(&select.distinct_on, resolver, &scope, &local_context)?,
        locking: select.locking.clone(),
    })
}

fn bind_from(
    from: &FromClause,
    resolver: &mut dyn VariableResolver,
    inherited: &RuleBindingScope,
    context: &RuleBindingContext<'_>,
) -> Result<FromClause, SQLError> {
    Ok(match from {
        FromClause::Table { .. } => from.clone(),
        FromClause::Join { .. } => bind_join_from(from, resolver, inherited, context)?,
        FromClause::Values {
            rows,
            alias,
            column_aliases,
            internal_relation,
            internal_column_types,
        } => FromClause::Values {
            rows: rows
                .iter()
                .map(|row| bind_exprs(row, resolver, inherited, context))
                .collect::<Result<Vec<_>, SQLError>>()?,
            alias: alias.clone(),
            column_aliases: column_aliases.clone(),
            internal_relation: *internal_relation,
            internal_column_types: internal_column_types.clone(),
        },
        FromClause::Function {
            name,
            output_name,
            relations,
            args,
            alias,
            column_aliases,
            ordinality,
            column_types,
        } => FromClause::Function {
            name: name.clone(),
            output_name: output_name.clone(),
            relations: relations.clone(),
            args: bind_exprs(args, resolver, inherited, context)?,
            alias: alias.clone(),
            column_aliases: column_aliases.clone(),
            ordinality: *ordinality,
            column_types: column_types.clone(),
        },
        FromClause::FunctionGroup {
            functions,
            alias,
            column_aliases,
            ordinality,
        } => FromClause::FunctionGroup {
            functions: functions
                .iter()
                .map(|function| {
                    Ok(uqa_sql::ast::TableFunction {
                        name: function.name.clone(),
                        output_name: function.output_name.clone(),
                        relations: function.relations.clone(),
                        args: bind_exprs(&function.args, resolver, inherited, context)?,
                        column_aliases: function.column_aliases.clone(),
                        column_types: function.column_types.clone(),
                    })
                })
                .collect::<Result<Vec<_>, SQLError>>()?,
            alias: alias.clone(),
            column_aliases: column_aliases.clone(),
            ordinality: *ordinality,
        },
        FromClause::Subquery {
            body,
            alias,
            column_aliases,
        } => FromClause::Subquery {
            body: Box::new(bind_select_with_scope(body, resolver, inherited, context)?),
            alias: alias.clone(),
            column_aliases: column_aliases.clone(),
        },
    })
}

fn bind_join_from(
    from: &FromClause,
    resolver: &mut dyn VariableResolver,
    inherited: &RuleBindingScope,
    context: &RuleBindingContext<'_>,
) -> Result<FromClause, SQLError> {
    let FromClause::Join {
        left,
        right,
        kind,
        on,
        using,
        natural,
        alias,
        column_aliases,
        lateral,
    } = from
    else {
        unreachable!("join binder received a non-join source")
    };
    let bound_left = bind_from(left, resolver, inherited, context)?;
    let mut right_scope = inherited.clone();
    if *lateral {
        collect_visible_scope(left, context, &mut right_scope)?;
    }
    let bound_right = bind_from(right, resolver, &right_scope, context)?;
    let mut on_scope = inherited.clone();
    collect_visible_scope(left, context, &mut on_scope)?;
    collect_visible_scope(right, context, &mut on_scope)?;
    Ok(FromClause::Join {
        left: Box::new(bound_left),
        right: Box::new(bound_right),
        kind: *kind,
        on: bind_optional_expr(on.as_ref(), resolver, &on_scope, context)?,
        using: using.clone(),
        natural: *natural,
        alias: alias.clone(),
        column_aliases: column_aliases.clone(),
        lateral: *lateral,
    })
}

fn bind_ctes(
    ctes: &[CTE],
    resolver: &mut dyn VariableResolver,
    inherited: &RuleBindingScope,
    context: &RuleBindingContext<'_>,
) -> Result<Vec<CTE>, SQLError> {
    let mut visible = context.clone();
    let mut bound = Vec::with_capacity(ctes.len());
    for cte in ctes {
        if cte.recursive {
            visible
                .ctes
                .entry(cte.name.to_ascii_lowercase())
                .or_default();
            let mut columns = select_output_columns(&cte.query, &visible)?;
            apply_positional_aliases(&mut columns, &cte.columns);
            visible.ctes.insert(cte.name.to_ascii_lowercase(), columns);
        }
        bound.push(CTE {
            name: cte.name.clone(),
            columns: cte.columns.clone(),
            recursive: cte.recursive,
            materialization: cte.materialization,
            search: cte.search.clone(),
            cycle: cte
                .cycle
                .as_ref()
                .map(|cycle| -> Result<uqa_sql::ast::CteCycleClause, SQLError> {
                    Ok(uqa_sql::ast::CteCycleClause {
                        columns: cycle.columns.clone(),
                        mark_column: cycle.mark_column.clone(),
                        mark_value: bind_rule_expr_with_scope(
                            &cycle.mark_value,
                            resolver,
                            inherited,
                            &visible,
                        )?,
                        mark_default: bind_rule_expr_with_scope(
                            &cycle.mark_default,
                            resolver,
                            inherited,
                            &visible,
                        )?,
                        path_column: cycle.path_column.clone(),
                    })
                })
                .transpose()?,
            query: Box::new(bind_select_with_scope(
                &cte.query, resolver, inherited, &visible,
            )?),
        });
        let mut columns = select_output_columns(&cte.query, &visible)?;
        apply_positional_aliases(&mut columns, &cte.columns);
        if let Some(search) = &cte.search {
            columns.push(search.sequence_column.clone());
        }
        if let Some(cycle) = &cte.cycle {
            columns.push(cycle.mark_column.clone());
            columns.push(cycle.path_column.clone());
        }
        visible.ctes.insert(cte.name.to_ascii_lowercase(), columns);
    }
    Ok(bound)
}

fn bind_frame_bound(
    bound: &FrameBound,
    resolver: &mut dyn VariableResolver,
    scope: &RuleBindingScope,
    context: &RuleBindingContext<'_>,
) -> Result<FrameBound, SQLError> {
    Ok(match bound {
        FrameBound::Preceding(expr) => FrameBound::Preceding(Box::new(bind_rule_expr_with_scope(
            expr, resolver, scope, context,
        )?)),
        FrameBound::Following(expr) => FrameBound::Following(Box::new(bind_rule_expr_with_scope(
            expr, resolver, scope, context,
        )?)),
        _ => bound.clone(),
    })
}

fn bind_projections(
    projections: &[Projection],
    resolver: &mut dyn VariableResolver,
    scope: &RuleBindingScope,
    context: &RuleBindingContext<'_>,
) -> Result<Vec<Projection>, SQLError> {
    let mut bound = Vec::with_capacity(projections.len());
    for projection in projections {
        if let Some(expressions) = expand_qualified_star(&projection.expr, resolver, scope)? {
            bound.extend(expressions.into_iter().map(|expr| Projection {
                expr,
                alias: projection.alias.clone(),
            }));
        } else {
            bound.push(Projection {
                expr: bind_rule_expr_with_scope(&projection.expr, resolver, scope, context)?,
                alias: projection.alias.clone(),
            });
        }
    }
    Ok(bound)
}

fn bind_expanding_exprs(
    expressions: &[Expr],
    resolver: &mut dyn VariableResolver,
    scope: &RuleBindingScope,
    context: &RuleBindingContext<'_>,
) -> Result<Vec<Expr>, SQLError> {
    let mut bound = Vec::with_capacity(expressions.len());
    for expression in expressions {
        if let Some(expressions) = expand_qualified_star(expression, resolver, scope)? {
            bound.extend(expressions);
        } else {
            bound.push(bind_rule_expr_with_scope(
                expression, resolver, scope, context,
            )?);
        }
    }
    Ok(bound)
}

fn expand_qualified_star(
    expression: &Expr,
    resolver: &mut dyn VariableResolver,
    scope: &RuleBindingScope,
) -> Result<Option<Vec<Expr>>, SQLError> {
    let Expr::QualifiedStar(qualifier) = expression else {
        return Ok(None);
    };
    if scope.qualifier_is_shadowed(qualifier) {
        return Ok(None);
    }
    resolver.rewrite_qualified_star(qualifier)
}

fn bind_exprs(
    expressions: &[Expr],
    resolver: &mut dyn VariableResolver,
    scope: &RuleBindingScope,
    context: &RuleBindingContext<'_>,
) -> Result<Vec<Expr>, SQLError> {
    expressions
        .iter()
        .map(|expr| bind_rule_expr_with_scope(expr, resolver, scope, context))
        .collect()
}

fn bind_optional_expr(
    expression: Option<&Expr>,
    resolver: &mut dyn VariableResolver,
    scope: &RuleBindingScope,
    context: &RuleBindingContext<'_>,
) -> Result<Option<Expr>, SQLError> {
    expression
        .map(|expr| bind_rule_expr_with_scope(expr, resolver, scope, context))
        .transpose()
}

fn bind_orders(
    orders: &[OrderBy],
    resolver: &mut dyn VariableResolver,
    scope: &RuleBindingScope,
    context: &RuleBindingContext<'_>,
) -> Result<Vec<OrderBy>, SQLError> {
    orders
        .iter()
        .map(|order| {
            Ok(OrderBy {
                expr: bind_rule_expr_with_scope(&order.expr, resolver, scope, context)?,
                descending: order.descending,
                nulls: order.nulls,
            })
        })
        .collect()
}

fn collect_visible_scope(
    from: &FromClause,
    context: &RuleBindingContext<'_>,
    output: &mut RuleBindingScope,
) -> Result<(), SQLError> {
    let names = source_names(from, context)?;
    for qualifier in names.qualified_columns.keys() {
        output.insert_qualifier(qualifier);
    }
    for column in names.columns {
        output.insert_column(&column);
    }
    Ok(())
}

fn source_names(
    from: &FromClause,
    context: &RuleBindingContext<'_>,
) -> Result<RuleSourceNames, SQLError> {
    match from {
        FromClause::Table {
            name,
            qualifier,
            alias,
            ..
        } => table_source_names(context, name, qualifier, alias.as_deref()),
        FromClause::Join {
            left,
            right,
            alias,
            column_aliases,
            using,
            natural,
            ..
        } => join_source_names(
            context,
            left,
            right,
            alias.as_deref(),
            column_aliases,
            using.as_ref(),
            *natural,
        ),
        FromClause::Values {
            rows,
            alias,
            column_aliases,
            ..
        } => Ok(aliased_source_names(
            (1..=rows.first().map_or(0, Vec::len))
                .map(|position| format!("column{position}"))
                .collect(),
            alias.as_deref(),
            column_aliases,
        )),
        FromClause::Subquery {
            body,
            alias,
            column_aliases,
        } => Ok(aliased_source_names(
            select_output_columns(body, context)?,
            alias.as_deref(),
            column_aliases,
        )),
        FromClause::Function {
            output_name,
            alias,
            column_aliases,
            ordinality,
            ..
        } => Ok(function_source_names(
            output_name,
            alias.as_deref(),
            column_aliases,
            *ordinality,
        )),
        FromClause::FunctionGroup {
            functions,
            alias,
            column_aliases,
            ordinality,
        } => Ok(function_group_source_names(
            functions,
            alias.as_deref(),
            column_aliases,
            *ordinality,
        )),
    }
}

fn table_source_names(
    context: &RuleBindingContext<'_>,
    name: &str,
    qualifier: &str,
    alias: Option<&str>,
) -> Result<RuleSourceNames, SQLError> {
    let columns = context.relation_columns(name)?;
    let mut names = RuleSourceNames {
        columns: columns.clone(),
        ..RuleSourceNames::default()
    };
    if let Some(alias) = alias {
        names.insert_qualifier(alias, &columns);
    } else {
        names.insert_qualifier(qualifier, &columns);
        names.insert_qualifier(name, &columns);
        if let Some((_, local)) = name.rsplit_once('.') {
            names.insert_qualifier(local.trim_matches('"'), &columns);
        }
    }
    Ok(names)
}

fn join_source_names(
    context: &RuleBindingContext<'_>,
    left: &FromClause,
    right: &FromClause,
    alias: Option<&str>,
    column_aliases: &[String],
    using: Option<&uqa_sql::ast::JoinUsing>,
    natural: bool,
) -> Result<RuleSourceNames, SQLError> {
    let left = source_names(left, context)?;
    let right = source_names(right, context)?;
    let merged = if natural {
        left.columns
            .iter()
            .filter(|column| contains_name(&right.columns, column))
            .cloned()
            .collect::<Vec<_>>()
    } else {
        using.map_or_else(Vec::new, |using| using.columns.clone())
    };
    let mut columns = merged.clone();
    columns.extend(
        left.columns
            .iter()
            .filter(|column| !contains_name(&merged, column))
            .cloned(),
    );
    columns.extend(
        right
            .columns
            .iter()
            .filter(|column| !contains_name(&merged, column))
            .cloned(),
    );
    apply_positional_aliases(&mut columns, column_aliases);
    let mut names = RuleSourceNames {
        columns: columns.clone(),
        ..RuleSourceNames::default()
    };
    if let Some(alias) = alias {
        names.insert_qualifier(alias, &columns);
    } else {
        names.qualified_columns.extend(left.qualified_columns);
        names.qualified_columns.extend(right.qualified_columns);
    }
    if let Some(alias) = using.and_then(|using| using.alias.as_deref()) {
        names.insert_qualifier(alias, &merged);
    }
    Ok(names)
}

fn aliased_source_names(
    mut columns: Vec<String>,
    alias: Option<&str>,
    column_aliases: &[String],
) -> RuleSourceNames {
    apply_positional_aliases(&mut columns, column_aliases);
    let mut names = RuleSourceNames {
        columns: columns.clone(),
        ..RuleSourceNames::default()
    };
    if let Some(alias) = alias {
        names.insert_qualifier(alias, &columns);
    }
    names
}

fn function_source_names(
    output_name: &str,
    alias: Option<&str>,
    column_aliases: &[String],
    ordinality: bool,
) -> RuleSourceNames {
    let mut columns = vec![output_name.to_string()];
    apply_positional_aliases(&mut columns, column_aliases);
    if ordinality {
        columns.push("ordinality".into());
    }
    let mut names = RuleSourceNames {
        columns: columns.clone(),
        ..RuleSourceNames::default()
    };
    names.insert_qualifier(alias.unwrap_or(output_name), &columns);
    names
}

fn function_group_source_names(
    functions: &[uqa_sql::ast::TableFunction],
    alias: Option<&str>,
    column_aliases: &[String],
    ordinality: bool,
) -> RuleSourceNames {
    let mut columns = functions
        .iter()
        .flat_map(function_output_columns)
        .collect::<Vec<_>>();
    apply_positional_aliases(&mut columns, column_aliases);
    if ordinality {
        columns.push("ordinality".into());
    }
    let mut names = RuleSourceNames {
        columns: columns.clone(),
        ..RuleSourceNames::default()
    };
    if let Some(alias) = alias {
        names.insert_qualifier(alias, &columns);
    } else {
        for function in functions {
            names.insert_qualifier(&function.output_name, &function_output_columns(function));
        }
    }
    names
}

fn function_output_columns(function: &uqa_sql::ast::TableFunction) -> Vec<String> {
    if function.column_aliases.is_empty() {
        vec![function.output_name.clone()]
    } else {
        function.column_aliases.clone()
    }
}

fn select_output_columns(
    select: &SelectStmt,
    context: &RuleBindingContext<'_>,
) -> Result<Vec<String>, SQLError> {
    let context = context.with_ctes(&select.with)?;
    if let Some(left) = select.set_op.as_ref().and_then(|set| set.left.as_deref()) {
        return select_output_columns(left, &context);
    }
    if select.projections.is_empty() && !select.values.is_empty() {
        return Ok((1..=select.values.first().map_or(0, Vec::len))
            .map(|position| format!("column{position}"))
            .collect());
    }
    let source = select
        .from
        .as_ref()
        .map(|source| source_names(source, &context))
        .transpose()?
        .unwrap_or_default();
    let mut columns = Vec::new();
    for projection in &select.projections {
        if let Some(alias) = &projection.alias {
            columns.push(alias.clone());
            continue;
        }
        match &projection.expr {
            Expr::Star => columns.extend(source.columns.iter().cloned()),
            Expr::QualifiedStar(qualifier) => {
                if let Some(qualified) = source.qualified(qualifier) {
                    columns.extend(qualified.iter().cloned());
                }
            }
            Expr::Column(column) | Expr::QualifiedColumn { column, .. } => {
                columns.push(column.clone());
            }
            Expr::Func { name, .. } => columns.push(
                name.rsplit_once('.')
                    .map_or(name.as_str(), |(_, local)| local)
                    .to_string(),
            ),
            _ => columns.push("?column?".into()),
        }
    }
    Ok(columns)
}

fn apply_positional_aliases(columns: &mut Vec<String>, aliases: &[String]) {
    if columns.is_empty() {
        columns.extend_from_slice(aliases);
        return;
    }
    for (position, alias) in aliases.iter().enumerate() {
        if let Some(column) = columns.get_mut(position) {
            column.clone_from(alias);
        }
    }
}

fn contains_name(columns: &[String], candidate: &str) -> bool {
    columns
        .iter()
        .any(|column| column.eq_ignore_ascii_case(candidate))
}
