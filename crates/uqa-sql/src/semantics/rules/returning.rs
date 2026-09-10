//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rule RETURNING image requirements, positional contracts, and action rewriting.
use crate::{
    ast::{Expr, Projection, ReturningAliases, RuleEvent, Statement},
    plan::{ProjectionPlan, QueryPlan, RelationalPlan, SourcePlan},
    plpgsql::{ResolvedVariable, VariableResolver},
    SQLError, ScalarExpr,
};
use std::collections::BTreeSet;
use uqa_core::Value;
#[derive(Clone, Copy, Default)]
pub struct RuleReturningRequest {
    capture: bool,
    images: RuleReturningImages,
}

#[derive(Clone, Copy, Default)]
struct RuleReturningImages(u8);

impl RuleReturningImages {
    const CURRENT: Self = Self(1 << 0);
    const OLD: Self = Self(1 << 1);
    const NEW: Self = Self(1 << 2);

    fn insert(&mut self, image: Self) {
        self.0 |= image.0;
    }

    const fn contains(self, image: Self) -> bool {
        self.0 & image.0 != 0
    }
}

impl RuleReturningRequest {
    pub fn from_plan(
        returning: &[ProjectionPlan],
        aliases: &ReturningAliases,
        subqueries: &[QueryPlan],
    ) -> Self {
        if returning.is_empty() {
            return Self::default();
        }
        let mut request = Self {
            capture: true,
            ..Self::default()
        };
        let shadowed = BTreeSet::new();
        for projection in returning {
            let ids = request.inspect_expression(&projection.expr, aliases, &shadowed);
            for id in ids {
                if let Some(query) = subqueries.get(id) {
                    request.inspect_query(query, aliases, &shadowed);
                }
            }
        }
        request
    }

    fn inspect_expression(
        &mut self,
        expression: &ScalarExpr,
        aliases: &ReturningAliases,
        shadowed: &BTreeSet<String>,
    ) -> Vec<usize> {
        let mut expression = expression.clone();
        let mut subqueries = Vec::new();
        crate::plan::rewrite_scalar_expression(&mut expression, &mut |node| match node {
            ScalarExpr::Star | ScalarExpr::Column(_) | ScalarExpr::Position(_) => {
                self.images.insert(RuleReturningImages::CURRENT);
            }
            ScalarExpr::QualifiedStar(qualifier)
            | ScalarExpr::QualifiedColumn { qualifier, .. }
                if !shadowed.contains(&qualifier.to_ascii_lowercase()) =>
            {
                if qualifier.eq_ignore_ascii_case(&aliases.old) {
                    self.images.insert(RuleReturningImages::OLD);
                } else if qualifier.eq_ignore_ascii_case(&aliases.new) {
                    self.images.insert(RuleReturningImages::NEW);
                } else {
                    self.images.insert(RuleReturningImages::CURRENT);
                }
            }
            ScalarExpr::ScalarSubquery(id)
            | ScalarExpr::Exists { subquery: id, .. }
            | ScalarExpr::InSubquery { subquery: id, .. } => subqueries.push(*id),
            _ => {}
        });
        subqueries
    }

    fn inspect_cte_body(
        &mut self,
        body: &crate::plan::CtePlanBody,
        aliases: &ReturningAliases,
        inherited: &BTreeSet<String>,
    ) {
        match body {
            crate::plan::CtePlanBody::Query(query) => self.inspect_query(query, aliases, inherited),
            crate::plan::CtePlanBody::Command(command) => {
                let mut scope = inherited.clone();
                if let Some(qualifier) = command.target_qualifier() {
                    scope.insert(qualifier.to_ascii_lowercase());
                }
                if let Some(source) = command.source_input() {
                    collect_source_qualifiers(source, &mut scope);
                    self.inspect_source(source, aliases, inherited);
                }
                if let Some(aliases) = command.returning_aliases() {
                    scope.insert(aliases.old.to_ascii_lowercase());
                    scope.insert(aliases.new.to_ascii_lowercase());
                }
                for cte in command.ctes() {
                    self.inspect_cte_body(&cte.body, aliases, &scope);
                }
                for query in command.query_inputs() {
                    self.inspect_query(query, aliases, &scope);
                }
                for expression in command.expressions() {
                    let _ = self.inspect_expression(expression, aliases, &scope);
                }
            }
        }
    }

    fn inspect_query(
        &mut self,
        query: &QueryPlan,
        aliases: &ReturningAliases,
        inherited: &BTreeSet<String>,
    ) {
        for cte in &query.ctes {
            self.inspect_cte_body(&cte.body, aliases, inherited);
        }
        match &query.root {
            RelationalPlan::QueryBlock(block) => {
                let mut scope = inherited.clone();
                if let Some(source) = &block.from {
                    collect_source_qualifiers(source, &mut scope);
                    self.inspect_source(source, aliases, inherited);
                }
                for expression in block
                    .projections
                    .iter()
                    .map(|projection| &projection.expr)
                    .chain(block.r#where.iter())
                    .chain(block.group_by.iter())
                    .chain(block.grouping_sets.iter().flatten())
                    .chain(block.having.iter())
                    .chain(block.order_by.iter().map(|order| &order.expr))
                    .chain(block.limit.iter())
                    .chain(block.offset.iter())
                    .chain(block.distinct_on.iter())
                {
                    let _ = self.inspect_expression(expression, aliases, &scope);
                }
                for subquery in &block.subqueries {
                    self.inspect_query(subquery, aliases, &scope);
                }
            }
            RelationalPlan::SetOp {
                left,
                right,
                order_by,
                limit,
                offset,
                subqueries,
                ..
            } => {
                self.inspect_query(left, aliases, inherited);
                self.inspect_query(right, aliases, inherited);
                for expression in order_by
                    .iter()
                    .map(|order| &order.expr)
                    .chain(limit.iter().map(Box::as_ref))
                    .chain(offset.iter().map(Box::as_ref))
                {
                    let _ = self.inspect_expression(expression, aliases, inherited);
                }
                for subquery in subqueries {
                    self.inspect_query(subquery, aliases, inherited);
                }
            }
            RelationalPlan::Values { rows, subqueries } => {
                for expression in rows.iter().flatten() {
                    let _ = self.inspect_expression(expression, aliases, inherited);
                }
                for subquery in subqueries {
                    self.inspect_query(subquery, aliases, inherited);
                }
            }
        }
    }

    fn inspect_source(
        &mut self,
        source: &SourcePlan,
        aliases: &ReturningAliases,
        inherited: &BTreeSet<String>,
    ) {
        match source {
            SourcePlan::Table { .. } => {}
            SourcePlan::Join {
                left,
                right,
                on,
                lateral,
                ..
            } => {
                self.inspect_source(left, aliases, inherited);
                let mut right_scope = inherited.clone();
                if *lateral {
                    collect_source_qualifiers(left, &mut right_scope);
                }
                self.inspect_source(right, aliases, &right_scope);
                if let Some(on) = on {
                    let mut scope = inherited.clone();
                    collect_source_qualifiers(left, &mut scope);
                    collect_source_qualifiers(right, &mut scope);
                    let _ = self.inspect_expression(on, aliases, &scope);
                }
            }
            SourcePlan::Values { rows, .. } => {
                for expression in rows.iter().flatten() {
                    let _ = self.inspect_expression(expression, aliases, inherited);
                }
            }
            SourcePlan::Function { args, .. } => {
                for expression in args {
                    let _ = self.inspect_expression(expression, aliases, inherited);
                }
            }
            SourcePlan::FunctionGroup { functions, .. } => {
                for expression in functions.iter().flat_map(|function| &function.args) {
                    let _ = self.inspect_expression(expression, aliases, inherited);
                }
            }
            SourcePlan::Subquery { body, .. } => self.inspect_query(body, aliases, inherited),
        }
    }

    pub const fn captures(self) -> bool {
        self.capture
    }
}

fn collect_source_qualifiers(source: &SourcePlan, output: &mut BTreeSet<String>) {
    match source {
        SourcePlan::Join {
            left, right, alias, ..
        } => {
            if let Some(alias) = alias {
                output.insert(alias.to_ascii_lowercase());
            } else {
                collect_source_qualifiers(left, output);
                collect_source_qualifiers(right, output);
            }
        }
        _ => {
            if let Some(qualifier) = source.visible_qualifier() {
                output.insert(qualifier.to_ascii_lowercase());
            }
        }
    }
}

pub fn validate_rule_returning_provider_width(
    provider_width: usize,
    event_width: usize,
) -> Result<(), SQLError> {
    if provider_width < event_width {
        return Err(SQLError::Internal(format!(
            "could not find replacement targetlist entry for attno {}",
            provider_width + 1
        )));
    }
    if provider_width > event_width {
        return Err(SQLError::Internal(format!(
            "rewrite-rule RETURNING provider produced {provider_width} columns, expected {event_width}"
        )));
    }
    Ok(())
}

pub fn augment_rule_returning_action(
    statement: &mut Statement,
    source_index: Option<Expr>,
    event_width: usize,
    request: RuleReturningRequest,
    target_columns: &BTreeSet<String>,
) -> Result<(), SQLError> {
    let (target_qualifier, aliases, returning) = match statement {
        Statement::Insert(action) => (
            action.target_qualifier.clone(),
            action.returning_aliases.clone(),
            action.returning.clone(),
        ),
        Statement::Update(action) => (
            action.target_qualifier.clone(),
            action.returning_aliases.clone(),
            action.returning.clone(),
        ),
        Statement::Delete(action) => (
            action.target_qualifier.clone(),
            action.returning_aliases.clone(),
            action.returning.clone(),
        ),
        _ => return Ok(()),
    };
    if returning.is_empty() {
        return Ok(());
    }
    let mut target_columns = target_columns.clone();
    target_columns.insert(crate::semantics::DOC_ID_COLUMN.into());
    let provider_event = match statement {
        Statement::Insert(_) => RuleEvent::Insert,
        Statement::Update(_) => RuleEvent::Update,
        Statement::Delete(_) => RuleEvent::Delete,
        _ => unreachable!("validated rule provider changed statement kind"),
    };
    let current = if request.images.contains(RuleReturningImages::CURRENT) {
        returning.clone()
    } else {
        null_rule_returning_image(event_width)
    };
    let old = if request.images.contains(RuleReturningImages::OLD)
        && provider_event != RuleEvent::Insert
    {
        rewrite_rule_returning_image(
            &returning,
            &target_qualifier,
            &target_columns,
            &aliases.old,
            &aliases.new,
            &aliases.old,
        )?
    } else {
        null_rule_returning_image(event_width)
    };
    let new = if request.images.contains(RuleReturningImages::NEW)
        && provider_event != RuleEvent::Delete
    {
        rewrite_rule_returning_image(
            &returning,
            &target_qualifier,
            &target_columns,
            &aliases.old,
            &aliases.new,
            &aliases.new,
        )?
    } else {
        null_rule_returning_image(event_width)
    };
    let output = match statement {
        Statement::Insert(action) => &mut action.returning,
        Statement::Update(action) => &mut action.returning,
        Statement::Delete(action) => &mut action.returning,
        _ => unreachable!("validated rule provider changed statement kind"),
    };
    *output = current;
    output.extend(old);
    output.extend(new);
    if let Some(expr) = source_index {
        output.push(Projection { expr, alias: None });
    }
    Ok(())
}

fn null_rule_returning_image(width: usize) -> Vec<Projection> {
    (0..width)
        .map(|_| Projection {
            expr: Expr::Literal(Value::Null),
            alias: None,
        })
        .collect()
}

fn rewrite_rule_returning_image(
    returning: &[Projection],
    target_qualifier: &str,
    target_columns: &BTreeSet<String>,
    old_qualifier: &str,
    new_qualifier: &str,
    image_qualifier: &str,
) -> Result<Vec<Projection>, SQLError> {
    let mut resolver = ReturningImageResolver {
        target_qualifier,
        target_columns,
        old_qualifier,
        new_qualifier,
        image_qualifier,
    };
    returning
        .iter()
        .map(|projection| {
            let expr = match &projection.expr {
                Expr::Star => Expr::QualifiedStar(image_qualifier.to_string()),
                Expr::QualifiedStar(qualifier) if resolver.retargets_qualifier(qualifier) => {
                    Expr::QualifiedStar(image_qualifier.to_string())
                }
                expr => super::action_binding::bind_rule_expr_scoped(
                    expr,
                    &mut resolver,
                    &BTreeSet::new(),
                )?,
            };
            Ok(Projection {
                expr,
                alias: projection.alias.clone(),
            })
        })
        .collect()
}

struct ReturningImageResolver<'a> {
    target_qualifier: &'a str,
    target_columns: &'a BTreeSet<String>,
    old_qualifier: &'a str,
    new_qualifier: &'a str,
    image_qualifier: &'a str,
}

impl ReturningImageResolver<'_> {
    fn retargets_qualifier(&self, qualifier: &str) -> bool {
        qualifier.eq_ignore_ascii_case(self.target_qualifier)
            || qualifier.eq_ignore_ascii_case(self.old_qualifier)
            || qualifier.eq_ignore_ascii_case(self.new_qualifier)
    }
}

impl VariableResolver for ReturningImageResolver<'_> {
    fn resolve_name(&mut self, _name: &str) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }

    fn resolve_qualified(
        &mut self,
        _qualifier: &str,
        _column: &str,
    ) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }

    fn resolve_param(&mut self, _index: usize) -> Result<Option<ResolvedVariable>, SQLError> {
        Ok(None)
    }

    fn rewrite_name(&mut self, name: &str) -> Result<Option<Expr>, SQLError> {
        Ok(self
            .target_columns
            .contains(name)
            .then(|| Expr::qualified_column(self.image_qualifier, name)))
    }

    fn rewrite_qualified(
        &mut self,
        qualifier: &str,
        column: &str,
    ) -> Result<Option<Expr>, SQLError> {
        Ok(self
            .retargets_qualifier(qualifier)
            .then(|| Expr::qualified_column(self.image_qualifier, column)))
    }
}
