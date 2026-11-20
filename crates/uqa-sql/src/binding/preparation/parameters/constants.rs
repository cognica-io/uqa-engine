//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    install_rewritten_block, AnalysisIdentity, InputConstants, OwnedScalarExpr, ProductionControl,
    SQLError, SQLParam, ScalarExpr, UnifiedPlan, Value,
};

pub(super) mod input;

impl InputConstants {
    /// Ordinary messages must repeat session/catalog-dependent input functions.
    /// Prepared definitions intentionally retain these same converted constants.
    pub(in crate::binding::preparation) fn reusable_across_messages(&self) -> bool {
        self.0
            .values()
            .chain(
                self.2
                    .values()
                    .flat_map(|membership| membership.left_constants.iter().flatten()),
            )
            .all(|input| !input.session_dependent)
    }

    fn apply_node(&mut self, expression: &mut ScalarExpr) -> Result<(), SQLError> {
        let identity = AnalysisIdentity::from(&*expression);
        let input = std::mem::replace(expression, ScalarExpr::Literal(Value::Null));
        let control = ProductionControl::uncontrolled();
        *expression = self
            .apply_node_owned(identity, OwnedScalarExpr::uncontrolled(input), &control)?
            .finish(&control)?
            .into_uncontrolled()
            .expect("ordinary input application");
        Ok(())
    }

    pub(super) fn apply_node_owned(
        &mut self,
        identity: AnalysisIdentity<ScalarExpr>,
        expression: OwnedScalarExpr,
        control: &ProductionControl<'_>,
    ) -> Result<OwnedScalarExpr, SQLError> {
        crate::catalog::work::inline(input::apply_from(
            &mut input::Inline(self),
            identity,
            expression,
            control,
            &mut (),
        ))
    }

    #[cfg(test)]
    pub(in crate::binding::preparation) fn apply_expression(
        self,
        expression: &mut ScalarExpr,
    ) -> Result<(), SQLError> {
        self.apply_expression_with_subqueries(
            expression,
            &mut Vec::new(),
            &super::super::tests::NoRoutines,
            &[],
        )
    }

    pub(in crate::binding::preparation) fn apply_expression_with_subqueries(
        mut self,
        expression: &mut ScalarExpr,
        subqueries: &mut Vec<crate::plan::QueryPlan>,
        routines: &dyn crate::routines::RoutineResolution,
        params: &[SQLParam],
    ) -> Result<(), SQLError> {
        for query in subqueries.iter_mut() {
            query.try_visit_queries_mut(&mut |query| self.apply_query(query))?;
            query.visit_sources_mut(&mut |source| self.apply_source(source));
        }
        let mut groupings = std::mem::take(&mut self.8.groupings);
        crate::plan::subqueries::rewrite_expression_with_arena_and_metadata(
            expression,
            subqueries,
            &mut |node, arena| {
                self.apply_membership(node, arena)?;
                self.apply_node(node)
            },
            &mut |query| groupings.apply_block(query, routines, params),
        )?;
        if self.is_consumed() && groupings.is_empty() {
            Ok(())
        } else {
            Err(SQLError::Internal(
                "default input constant did not belong to the analyzed expression".into(),
            ))
        }
    }

    pub(in crate::binding::preparation) fn apply(
        mut self,
        plan: &mut UnifiedPlan,
        routines: &dyn crate::routines::RoutineResolution,
        params: &[SQLParam],
    ) -> Result<(), SQLError> {
        plan.try_visit_queries_mut(&mut |query| self.apply_query(query))?;
        if !self.11.is_empty() {
            plan.apply_projection_aliases(&mut |projections| {
                self.projection_aliases(projections, &ProductionControl::uncontrolled())
            })?;
        }
        // Source selections are consumed before expression rewrites can move their enclosing nodes.
        plan.visit_sources_mut(&mut |source| self.apply_source(source));
        if self.is_consumed() {
            return Ok(());
        }
        // Consume all selected inputs at their original address before a parent transfers its children.
        let mut groupings = std::mem::take(&mut self.8.groupings);
        crate::plan::subqueries::rewrite_with_arenas_and_metadata(
            plan,
            &mut |node, arena| {
                self.apply_membership(node, arena)?;
                self.apply_node(node)
            },
            &mut |query| groupings.apply_block(query, routines, params),
        )?;
        if self.is_consumed() && groupings.is_empty() {
            Ok(())
        } else {
            Err(SQLError::Internal(
                "prepared input constant did not belong to the analyzed plan".into(),
            ))
        }
    }

    pub(super) fn is_consumed(&self) -> bool {
        self.0.is_empty()
            && self.1.is_empty()
            && self.2.is_empty()
            && self.3.is_empty()
            && self.4.is_empty()
            && self.5.is_empty()
            && self.6.is_empty()
            && self.7.is_empty()
            && self.8.outputs.is_empty()
            && self.8.sources.is_empty()
            && self.8.groupings.is_empty()
            && self.9.is_empty()
            && self.10.is_empty()
            && self.11.is_empty()
            && !self.12
    }

    fn apply_query(&mut self, query: &mut crate::plan::QueryPlan) -> Result<(), SQLError> {
        install_rewritten_block(&mut self.10, query)?;
        self.8.apply_query(query)
    }

    fn apply_source(&mut self, source: &mut crate::plan::SourcePlan) {
        // Command inputs have no enclosing QueryPlan; their validated descriptors are not needed by query-output pruning.
        self.8.sources.remove(&AnalysisIdentity::from(&*source));
        let mut apply = |binding: &mut Option<crate::ast::FunctionBinding>| {
            if let Some(selected) = self.7.remove(&AnalysisIdentity::from(&*binding)) {
                *binding = Some(
                    selected
                        .into_uncontrolled()
                        .expect("ordinary source binding application"),
                );
            }
        };
        match source {
            crate::plan::SourcePlan::Function { binding, .. } => apply(binding),
            crate::plan::SourcePlan::FunctionGroup { functions, .. } => {
                for function in functions {
                    apply(&mut function.binding);
                }
            }
            _ => {}
        }
    }

    fn apply_membership(
        &mut self,
        node: &mut ScalarExpr,
        arena: &mut Vec<crate::plan::QueryPlan>,
    ) -> Result<(), SQLError> {
        if let Some(analysis) = self.2.remove(&AnalysisIdentity::from(&*node)) {
            analysis.apply(node, arena)?;
        }
        Ok(())
    }
}
