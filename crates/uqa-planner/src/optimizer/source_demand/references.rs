//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resolve consumed values against their lexical query and preceding-source scopes.

use uqa_sql::plan::AnalyzedSource;
use uqa_sql::schema::InputSlots;
use uqa_sql::{RowSchema, SQLError, ScalarExpr};

use super::{Demand, QueryPlan, SourcePlan};

pub(super) struct References<'a, 'c> {
    target: InputSlots<'a, 'c>,
    demand: &'a mut Demand<'c>,
}

impl<'a, 'c> References<'a, 'c> {
    pub(super) fn new(target: &'a RowSchema, demand: &'a mut Demand<'c>) -> Self {
        Self {
            target: InputSlots::borrow(target, demand.control),
            demand,
        }
    }

    pub(super) fn join(
        left: &'a RowSchema,
        right: &'a RowSchema,
        demand: &'a mut Demand<'c>,
    ) -> Result<Self, SQLError> {
        Ok(Self {
            target: InputSlots::join(left, right, demand.control)?,
            demand,
        })
    }

    pub(super) fn expression(
        &mut self,
        expression: &ScalarExpr,
        subqueries: &[QueryPlan],
    ) -> Result<(), SQLError> {
        let control = self.demand.control;
        let mut visitor = |part: &ScalarExpr| {
            self.target
                .visit_reference(part, &mut |slot| self.demand.extend([slot]))
        };
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
        let mut visitor = |part: &ScalarExpr| {
            self.target
                .visit_reference(part, &mut |slot| self.demand.extend([slot]))
        };
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
