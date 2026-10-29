//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resolve consumed values against their lexical query and preceding-source scopes.

use uqa_sql::plan::AnalyzedSource;
use uqa_sql::{RowSchema, SQLError, ScalarExpr};

use super::{Demand, QueryPlan, SourcePlan};

pub(super) struct References<'a, 'c> {
    target: &'a RowSchema,
    demand: &'a mut Demand<'c>,
}

impl<'a, 'c> References<'a, 'c> {
    pub(super) fn new(target: &'a RowSchema, demand: &'a mut Demand<'c>) -> Self {
        Self { target, demand }
    }

    pub(super) fn expression(
        &mut self,
        expression: &ScalarExpr,
        subqueries: &[QueryPlan],
    ) -> Result<(), SQLError> {
        let control = self.demand.control;
        let mut visitor = |part: &ScalarExpr| retain(part, self.target, self.demand);
        let mut references = uqa_sql::binding::scoped_references::ControlledScopedReferences::new(
            &mut visitor,
            control,
        );
        references.expression(expression, subqueries)?;
        if !references.is_complete() {
            self.demand.extend(0..self.target.physical_width())?;
        }
        Ok(())
    }

    pub(super) fn source(
        &mut self,
        source: &SourcePlan,
        analysis: Option<&AnalyzedSource>,
        subqueries: &[QueryPlan],
        preceding: &[RowSchema],
        inherited_lateral: bool,
    ) -> Result<(), SQLError> {
        let control = self.demand.control;
        let mut visitor = |part: &ScalarExpr| retain(part, self.target, self.demand);
        let mut references = uqa_sql::binding::scoped_references::ControlledScopedReferences::new(
            &mut visitor,
            control,
        );
        references.source(source, analysis, subqueries, preceding, inherited_lateral)?;
        if !references.is_complete() {
            self.demand.extend(0..self.target.physical_width())?;
        }
        Ok(())
    }
}

pub(super) use uqa_sql::binding::scoped_references::visit_query_inputs_with_control;

fn retain(
    expression: &ScalarExpr,
    schema: &RowSchema,
    demand: &mut Demand<'_>,
) -> Result<(), SQLError> {
    let slot = match expression {
        ScalarExpr::Column(name) => {
            if let Some(slot) = schema.column_slot(name) {
                Some(slot)
            } else {
                demand.extend(schema.qualified_star_slots(name))?;
                None
            }
        }
        ScalarExpr::QualifiedColumn { qualifier, column } => {
            schema.qualified_slot(qualifier, column)
        }
        ScalarExpr::InternalColumn(column) => schema.internal_slot(*column),
        ScalarExpr::QualifiedStar(qualifier) => {
            demand.extend(schema.qualified_star_slots(qualifier))?;
            None
        }
        ScalarExpr::Star => {
            demand.extend(
                (0..schema.len())
                    .filter(|position| schema.wildcard_position_visible(*position))
                    .filter_map(|position| schema.slot(position)),
            )?;
            None
        }
        ScalarExpr::Position(position) => schema.slot(*position),
        _ => None,
    };
    demand.extend(slot)
}
