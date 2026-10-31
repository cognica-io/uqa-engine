//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lexical frames borrow retained schemas and preceding sources for the duration of observation.

use super::{
    visit_query_inputs_with_control, AnalyzedSource, QueryPlan, RowSchema, SQLError, ScalarExpr,
    SourcePlan,
};
use crate::plan::RelationalPlan;
use uqa_core::memory::ProductionControl;

/// Fallible reference observation shares the ordinary lexical traversal while preserving the original allowance and both cancellation owners.
pub struct ControlledScopedReferences<'v, 'c> {
    visitor: &'v mut dyn FnMut(&ScalarExpr) -> Result<(), SQLError>,
    complete: bool,
    control: ProductionControl<'c>,
}

impl<'v, 'c> ControlledScopedReferences<'v, 'c> {
    pub fn new(
        visitor: &'v mut dyn FnMut(&ScalarExpr) -> Result<(), SQLError>,
        control: ProductionControl<'c>,
    ) -> Self {
        Self {
            visitor,
            complete: true,
            control,
        }
    }

    pub fn is_complete(&self) -> bool {
        self.complete
    }

    pub fn expression(
        &mut self,
        expression: &ScalarExpr,
        subqueries: &[QueryPlan],
    ) -> Result<(), SQLError> {
        self.expression_in(expression, subqueries, &Scope::Empty)
    }

    pub fn query(&mut self, query: &QueryPlan) -> Result<(), SQLError> {
        self.query_in(query, &Scope::Empty)
    }

    pub fn source(
        &mut self,
        source: &SourcePlan,
        analysis: Option<&AnalyzedSource>,
        subqueries: &[QueryPlan],
        preceding: &[RowSchema],
        inherited_lateral: bool,
    ) -> Result<(), SQLError> {
        self.source_in(
            source,
            analysis,
            subqueries,
            &Scope::Slice(preceding),
            inherited_lateral,
            &Scope::Empty,
        )
    }

    fn expression_in(
        &mut self,
        expression: &ScalarExpr,
        subqueries: &[QueryPlan],
        scopes: &Scope<'_>,
    ) -> Result<(), SQLError> {
        let control = self.control;
        crate::semantics::try_visit_input_values_with_control(expression, &control, &mut |part| {
            let index = match part {
                ScalarExpr::ScalarSubquery(index)
                | ScalarExpr::Exists {
                    subquery: index, ..
                }
                | ScalarExpr::InSubquery {
                    subquery: index, ..
                } => Some(*index),
                _ => None,
            };
            if let Some(index) = index {
                if let Some(query) = subqueries.get(index) {
                    self.query_in(query, scopes)?;
                } else {
                    self.complete = false;
                }
            } else {
                let reference = crate::type_resolution::field_selection::field_reference(part)
                    .map_or(part, |(base, _)| base);
                if !scopes.contains(reference, &control)? {
                    (self.visitor)(part)?;
                }
            }
            Ok(())
        })
    }

    fn query_in(&mut self, query: &QueryPlan, scopes: &Scope<'_>) -> Result<(), SQLError> {
        self.control.check()?;
        for cte in &query.ctes {
            self.control.check()?;
            if let Some(query) = cte.body.query() {
                self.query_in(query, scopes)?;
            } else {
                self.complete = false;
            }
        }
        match &query.root {
            RelationalPlan::QueryBlock(block) => {
                if let Some(input) = &block.from {
                    self.complete &= query.analyzed_source.is_some();
                    self.source_in(
                        input,
                        query.analyzed_source.as_deref(),
                        &block.subqueries,
                        &Scope::Empty,
                        false,
                        scopes,
                    )?;
                }
                let local = Scope::Schema(
                    query.analyzed_source.as_ref().map(|source| &source.schema),
                    scopes,
                );
                let control = self.control;
                visit_query_inputs_with_control(block, &control, &mut |expression| {
                    self.expression_in(expression, &block.subqueries, &local)
                })?;
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
                self.query_in(left, scopes)?;
                self.query_in(right, scopes)?;
                self.complete &= query.analyzed_output.is_some();
                let local = Scope::Schema(query.analyzed_output.as_ref(), scopes);
                for expression in order_by
                    .iter()
                    .map(|order| &order.expr)
                    .chain(limit.as_deref())
                    .chain(offset.as_deref())
                {
                    self.expression_in(expression, subqueries, &local)?;
                }
            }
            RelationalPlan::Values { rows, subqueries } => {
                for expression in rows.iter().flatten() {
                    self.expression_in(expression, subqueries, scopes)?;
                }
            }
        }
        self.control.check()?;
        Ok(())
    }

    fn source_in(
        &mut self,
        source: &SourcePlan,
        analysis: Option<&AnalyzedSource>,
        subqueries: &[QueryPlan],
        preceding: &Scope<'_>,
        inherited_lateral: bool,
        scopes: &Scope<'_>,
    ) -> Result<(), SQLError> {
        self.control.check()?;
        let lateral = Scope::Combined(preceding, scopes);
        let scopes = if inherited_lateral
            || source.is_explicitly_lateral()
            || matches!(
                source,
                SourcePlan::Function { .. } | SourcePlan::FunctionGroup { .. }
            ) {
            &lateral
        } else {
            scopes
        };
        match source {
            SourcePlan::Join {
                left,
                right,
                on,
                lateral,
                ..
            } => {
                if let Some(children) = analysis.and_then(|source| source.children.as_deref()) {
                    self.source_in(
                        left,
                        Some(&children.0),
                        subqueries,
                        preceding,
                        false,
                        scopes,
                    )?;
                    let right_preceding = Scope::Schema(Some(&children.0.schema), preceding);
                    self.source_in(
                        right,
                        Some(&children.1),
                        subqueries,
                        &right_preceding,
                        *lateral,
                        scopes,
                    )?;
                    if let Some(on) = on {
                        let left = Scope::Schema(Some(&children.0.schema), scopes);
                        let right = Scope::Schema(Some(&children.1.schema), &left);
                        self.expression_in(on, subqueries, &right)?;
                    }
                } else {
                    self.complete = false;
                }
            }
            SourcePlan::Subquery { body, .. }
            | SourcePlan::Table {
                executable_query: Some(body),
                ..
            } => self.query_in(body, scopes)?,
            SourcePlan::Values { rows, .. } => {
                for expression in rows.iter().flatten() {
                    self.expression_in(expression, subqueries, scopes)?;
                }
            }
            SourcePlan::Function { args, .. } => {
                for expression in args {
                    self.expression_in(expression, subqueries, scopes)?;
                }
            }
            SourcePlan::FunctionGroup { functions, .. } => {
                for expression in functions.iter().flat_map(|function| &function.args) {
                    self.expression_in(expression, subqueries, scopes)?;
                }
            }
            SourcePlan::RetainedRows(_) | SourcePlan::Table { .. } => {}
        }
        self.control.check()?;
        Ok(())
    }
}

enum Scope<'a> {
    Empty,
    Schema(Option<&'a RowSchema>, &'a Self),
    Slice(&'a [RowSchema]),
    Combined(&'a Self, &'a Self),
}

impl Scope<'_> {
    fn contains(
        &self,
        expression: &ScalarExpr,
        control: &ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        let mut scope = self;
        loop {
            control.check()?;
            match scope {
                Self::Empty => return Ok(false),
                Self::Schema(schema, parent) => {
                    if locally_resolved(expression, *schema) {
                        return Ok(true);
                    }
                    scope = parent;
                }
                Self::Slice(schemas) => {
                    for schema in schemas.iter().rev() {
                        control.check()?;
                        if locally_resolved(expression, Some(schema)) {
                            return Ok(true);
                        }
                    }
                    return Ok(false);
                }
                Self::Combined(first, second) => {
                    if first.contains(expression, control)? {
                        return Ok(true);
                    }
                    scope = second;
                }
            }
        }
    }
}

fn locally_resolved(expression: &ScalarExpr, schema: Option<&RowSchema>) -> bool {
    // A present query frame resolves stars and ordinals even when its source descriptor is empty.
    if matches!(expression, ScalarExpr::Star | ScalarExpr::Position(_)) {
        return true;
    }
    let Some(schema) = schema else {
        return false;
    };
    match expression {
        ScalarExpr::Column(name) => {
            schema.resolves_local_column(None, name) || schema.has_qualifier(name)
        }
        ScalarExpr::QualifiedColumn { qualifier, .. } => schema.has_qualifier(qualifier),
        ScalarExpr::QualifiedStar(qualifier) => schema.has_qualifier(qualifier),
        ScalarExpr::InternalColumn(column) => schema.internal_slot(*column).is_some(),
        _ => false,
    }
}
