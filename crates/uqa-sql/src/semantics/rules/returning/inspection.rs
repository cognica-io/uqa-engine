//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! RETURNING image selection borrows expressions and lexical qualifiers without copying payloads or building name indexes.

use super::{RuleReturningImages, RuleReturningRequest};
use crate::{
    ast::ReturningAliases,
    plan::{CtePlanBody, ProjectionPlan, QueryPlan, RelationalPlan, SourcePlan},
    SQLError, ScalarExpr,
};
use uqa_core::memory::ProductionControl;

impl RuleReturningRequest {
    pub fn from_plan(
        returning: &[ProjectionPlan],
        aliases: &ReturningAliases,
        subqueries: &[QueryPlan],
    ) -> Self {
        Self::from_plan_with_control(
            returning,
            aliases,
            subqueries,
            &ProductionControl::uncontrolled(),
        )
        .expect("ordinary image inspection cannot fail")
    }

    pub fn from_plan_with_control(
        returning: &[ProjectionPlan],
        aliases: &ReturningAliases,
        subqueries: &[QueryPlan],
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        let mut inspection = Inspection {
            request: Self {
                capture: !returning.is_empty(),
                ..Self::default()
            },
            aliases,
            control: *control,
        };
        let scope = Scope::default();
        for projection in returning {
            inspection.expression(&projection.expr, &scope)?;
            // Follow the same selected IDs after visiting the complete projection; other query blocks own and visit their full subquery list separately.
            projection.expr.try_visit(&mut |node| {
                if let ScalarExpr::ScalarSubquery(id)
                | ScalarExpr::Exists { subquery: id, .. }
                | ScalarExpr::InSubquery { subquery: id, .. } = node
                {
                    if let Some(query) = subqueries.get(*id) {
                        inspection.query(query, &scope)?;
                    }
                }
                control.check()?;
                Ok::<_, SQLError>(true)
            })?;
        }
        Ok(inspection.request)
    }

    pub const fn captures(self) -> bool {
        self.capture
    }
}

#[derive(Default)]
struct Scope<'a> {
    parent: Option<&'a Scope<'a>>,
    source: Option<&'a SourcePlan>,
    target: Option<&'a str>,
    aliases: Option<&'a ReturningAliases>,
}
impl Scope<'_> {
    fn contains(&self, name: &str, control: &ProductionControl<'_>) -> Result<bool, SQLError> {
        control.check()?;
        if self
            .target
            .is_some_and(|target| target.eq_ignore_ascii_case(name))
            || self.aliases.is_some_and(|aliases| {
                aliases.old.eq_ignore_ascii_case(name) || aliases.new.eq_ignore_ascii_case(name)
            })
        {
            return Ok(true);
        }
        if let Some(source) = self.source {
            if source_exports(source, name, control)? {
                return Ok(true);
            }
        }
        self.parent
            .map_or(Ok(false), |parent| parent.contains(name, control))
    }
}

fn source_exports(
    source: &SourcePlan,
    name: &str,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    control.check()?;
    match source {
        SourcePlan::Join {
            left, right, alias, ..
        } => {
            if let Some(alias) = alias {
                Ok(alias.eq_ignore_ascii_case(name))
            } else {
                Ok(source_exports(left, name, control)? || source_exports(right, name, control)?)
            }
        }
        _ => Ok(source
            .visible_qualifier()
            .is_some_and(|qualifier| qualifier.eq_ignore_ascii_case(name))),
    }
}

struct Inspection<'a> {
    request: RuleReturningRequest,
    aliases: &'a ReturningAliases,
    control: ProductionControl<'a>,
}
impl Inspection<'_> {
    fn expression(&mut self, expression: &ScalarExpr, scope: &Scope<'_>) -> Result<(), SQLError> {
        expression.try_visit(&mut |node| {
            self.control.check()?;
            match node {
                ScalarExpr::Star | ScalarExpr::Column(_) | ScalarExpr::Position(_) => {
                    self.request.images.insert(RuleReturningImages::CURRENT);
                }
                ScalarExpr::QualifiedStar(_) | ScalarExpr::QualifiedColumn { .. } => {
                    let qualifier = node.reference_qualifier().expect("qualified reference");
                    if !scope.contains(qualifier, &self.control)? {
                        self.request.images.insert(
                            if qualifier.eq_ignore_ascii_case(&self.aliases.old) {
                                RuleReturningImages::OLD
                            } else if qualifier.eq_ignore_ascii_case(&self.aliases.new) {
                                RuleReturningImages::NEW
                            } else {
                                RuleReturningImages::CURRENT
                            },
                        );
                    }
                }
                _ => {}
            }
            Ok(true)
        })
    }

    fn cte_body(&mut self, body: &CtePlanBody, inherited: &Scope<'_>) -> Result<(), SQLError> {
        self.control.check()?;
        match body {
            CtePlanBody::Query(query) => self.query(query, inherited),
            CtePlanBody::Command(command) => {
                let scope = Scope {
                    parent: Some(inherited),
                    source: command.source_input(),
                    target: command.target_qualifier(),
                    aliases: command.returning_aliases(),
                };
                if let Some(source) = command.source_input() {
                    self.source(source, inherited)?;
                }
                for cte in command.ctes() {
                    self.cte_body(&cte.body, &scope)?;
                }
                for query in command.query_inputs() {
                    self.query(query, &scope)?;
                }
                for expression in command.expressions() {
                    self.expression(expression, &scope)?;
                }
                Ok(())
            }
        }
    }

    fn query(&mut self, query: &QueryPlan, inherited: &Scope<'_>) -> Result<(), SQLError> {
        self.control.check()?;
        for cte in &query.ctes {
            self.cte_body(&cte.body, inherited)?;
        }
        match &query.root {
            RelationalPlan::QueryBlock(block) => {
                let scope = Scope {
                    parent: Some(inherited),
                    source: block.from.as_ref(),
                    ..Scope::default()
                };
                if let Some(source) = &block.from {
                    self.source(source, inherited)?;
                }
                for expression in block
                    .projections
                    .iter()
                    .map(|projection| &projection.expr)
                    .chain(block.r#where.iter())
                    .chain(&block.group_by)
                    .chain(block.grouping_sets.iter().flatten())
                    .chain(block.having.iter())
                    .chain(block.order_by.iter().map(|order| &order.expr))
                    .chain(block.limit.iter())
                    .chain(block.offset.iter())
                    .chain(&block.distinct_on)
                {
                    self.expression(expression, &scope)?;
                }
                for subquery in &block.subqueries {
                    self.query(subquery, &scope)?;
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
                self.query(left, inherited)?;
                self.query(right, inherited)?;
                for expression in order_by
                    .iter()
                    .map(|order| &order.expr)
                    .chain(limit.as_deref())
                    .chain(offset.as_deref())
                {
                    self.expression(expression, inherited)?;
                }
                for subquery in subqueries {
                    self.query(subquery, inherited)?;
                }
            }
            RelationalPlan::Values { rows, subqueries } => {
                for expression in rows.iter().flatten() {
                    self.expression(expression, inherited)?;
                }
                for subquery in subqueries {
                    self.query(subquery, inherited)?;
                }
            }
        }
        Ok(())
    }

    fn source(&mut self, source: &SourcePlan, inherited: &Scope<'_>) -> Result<(), SQLError> {
        self.control.check()?;
        match source {
            SourcePlan::RetainedRows(_) | SourcePlan::Table { .. } => {}
            SourcePlan::Join {
                left,
                right,
                on,
                lateral,
                ..
            } => {
                self.source(left, inherited)?;
                let right_scope = Scope {
                    parent: Some(inherited),
                    source: lateral.then_some(left),
                    ..Scope::default()
                };
                self.source(right, &right_scope)?;
                if let Some(on) = on {
                    let left_scope = Scope {
                        parent: Some(inherited),
                        source: Some(left),
                        ..Scope::default()
                    };
                    let scope = Scope {
                        parent: Some(&left_scope),
                        source: Some(right),
                        ..Scope::default()
                    };
                    self.expression(on, &scope)?;
                }
            }
            SourcePlan::Values { rows, .. } => {
                for expression in rows.iter().flatten() {
                    self.expression(expression, inherited)?;
                }
            }
            SourcePlan::Function { args, .. } => {
                for expression in args {
                    self.expression(expression, inherited)?;
                }
            }
            SourcePlan::FunctionGroup { functions, .. } => {
                for expression in functions.iter().flat_map(|function| &function.args) {
                    self.expression(expression, inherited)?;
                }
            }
            SourcePlan::Subquery { body, .. } => self.query(body, inherited)?,
        }
        Ok(())
    }
}
