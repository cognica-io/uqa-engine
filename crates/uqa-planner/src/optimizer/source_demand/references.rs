//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resolve consumed values against their lexical query and preceding-source scopes.

use uqa_sql::plan::AnalyzedSource;
use uqa_sql::{RowSchema, ScalarExpr};

use super::{Demand, QueryPlan, SourcePlan};

pub(super) struct References<'a> {
    target: &'a RowSchema,
    demand: &'a mut Demand,
}

impl<'a> References<'a> {
    pub(super) fn new(target: &'a RowSchema, demand: &'a mut Demand) -> Self {
        Self { target, demand }
    }

    pub(super) fn expression(&mut self, expression: &ScalarExpr, subqueries: &[QueryPlan]) {
        let mut visitor = |part: &ScalarExpr| retain(part, self.target, self.demand);
        let mut references =
            uqa_sql::binding::scoped_references::ScopedReferences::new(&mut visitor);
        references.expression(expression, subqueries);
        if !references.is_complete() {
            self.demand.extend(0..self.target.physical_width());
        }
    }

    pub(super) fn source(
        &mut self,
        source: &SourcePlan,
        analysis: Option<&AnalyzedSource>,
        subqueries: &[QueryPlan],
        preceding: &[RowSchema],
        inherited_lateral: bool,
    ) {
        let mut visitor = |part: &ScalarExpr| retain(part, self.target, self.demand);
        let mut references =
            uqa_sql::binding::scoped_references::ScopedReferences::new(&mut visitor);
        references.source(source, analysis, subqueries, preceding, inherited_lateral);
        if !references.is_complete() {
            self.demand.extend(0..self.target.physical_width());
        }
    }
}

pub(super) use uqa_sql::binding::scoped_references::query_inputs;

fn retain(expression: &ScalarExpr, schema: &RowSchema, demand: &mut Demand) {
    let slot = match expression {
        ScalarExpr::Column(name) => {
            if let Some(slot) = schema.column_slot(name) {
                Some(slot)
            } else {
                demand.extend(
                    schema
                        .qualified_star_position_layout(name)
                        .into_iter()
                        .map(|(_, _, slot, _)| slot),
                );
                None
            }
        }
        ScalarExpr::QualifiedColumn { qualifier, column } => {
            schema.qualified_slot(qualifier, column)
        }
        ScalarExpr::InternalColumn(column) => schema.internal_slot(*column),
        ScalarExpr::QualifiedStar(qualifier) => {
            demand.extend(
                schema
                    .qualified_star_position_layout(qualifier)
                    .into_iter()
                    .map(|(_, _, slot, _)| slot),
            );
            None
        }
        ScalarExpr::Star => {
            demand.extend(
                (0..schema.len())
                    .filter(|position| schema.wildcard_position_visible(*position))
                    .filter_map(|position| schema.slot(position)),
            );
            None
        }
        ScalarExpr::Position(position) => schema.slot(*position),
        _ => None,
    };
    demand.extend(slot);
}
