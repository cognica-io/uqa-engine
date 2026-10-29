//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    install_rewritten_block, AnalysisVec, InputConstants, NonNull, OwnedScalarExpr,
    ProductionControl, SQLError, SQLParam, ScalarExpr, UnifiedPlan, Value,
};

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
        let identity = NonNull::from(&*expression);
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
        identity: NonNull<ScalarExpr>,
        mut expression: OwnedScalarExpr,
        control: &ProductionControl<'_>,
    ) -> Result<OwnedScalarExpr, SQLError> {
        control.check()?;
        if let Some(index) = self.11.remove(&identity) {
            expression = ScalarExpr::Param(index).copy_for_rewrite(control)?;
        }
        if let Some(expanded) = self.9.remove(&identity) {
            if !matches!(&*expression, ScalarExpr::Row(_)) {
                return Err(SQLError::Internal(
                    "analyzed ROW expansion lost its constructor".into(),
                ));
            }
            let mut selected = AnalysisVec::new(control);
            let extra = expanded
                .iter()
                .map(|(_, fields)| fields.len())
                .sum::<usize>();
            selected.reserve(
                expression.child_count().saturating_sub(expanded.len()) + extra,
                control,
            )?;
            let mut expanded = expanded.into_iter().peekable();
            for (position, item) in expression.into_children().enumerate() {
                if expanded.peek().is_some_and(|(index, _)| *index == position) {
                    let (_, fields) = expanded.next().expect("selected ROW expansion");
                    let (mut fields, memory) = fields.into_parts();
                    for field in &mut fields {
                        field.transform_retained_nodes(control, &mut |identity, field| {
                            self.apply_node_owned(identity, field, control)
                        })?;
                    }
                    let fields = control.finish(fields, memory)?;
                    for field in super::super::ownership::AnalysisItems::from_produced(fields) {
                        selected.push(field, control)?;
                    }
                } else {
                    selected.push(item, control)?;
                }
            }
            expression = OwnedScalarExpr::row(selected.into_iter(), control)?;
        }
        if let Some(selected) = self.1.remove(&identity) {
            if matches!(&*expression, ScalarExpr::Func { .. }) {
                expression = expression.replace_call_binding(Some(selected), control)?;
                expression = crate::type_resolution::json_path::normalize_retained_owned(
                    expression, control,
                )?;
            }
        }
        if let Some(collation) = self.6.remove(&identity) {
            if matches!(
                &*expression,
                ScalarExpr::Func {
                    binding: Some(_),
                    ..
                }
            ) {
                expression = expression.set_call_input_collation(collation, control)?;
            }
        }
        if let Some(constant) = self.0.remove(&identity) {
            expression = constant.expression;
        }
        expression = self.3.apply_owned(identity, expression, control)?;
        expression = self.4.apply_owned(identity, expression, control)?;
        if let Some(target) = self.5.remove(&identity) {
            expression = OwnedScalarExpr::cast(
                expression,
                target.catalog_name_with_control(control)?,
                true,
                control,
            )?;
        }
        control.check()?;
        Ok(expression)
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
            query.visit_queries_mut(&mut |query| self.apply_query(query));
            query.visit_sources_mut(&mut |source| self.apply_source(source));
        }
        let mut result = Ok(());
        let mut apply = |node: &mut ScalarExpr| {
            if result.is_ok() {
                result = self.apply_node(node);
            }
        };
        crate::plan::rewrite_scalar_expression(expression, &mut apply);
        for query in subqueries.iter_mut() {
            query.rewrite_scalar_expressions(&mut apply);
        }
        result?;
        for query in subqueries.iter_mut() {
            self.8.groupings.apply_query(query, routines, params)?;
        }
        if !self.2.is_empty() {
            crate::plan::subqueries::rewrite_expression_with_arena(
                expression,
                subqueries,
                &mut |node, arena| self.apply_membership(node, arena),
            )?;
        }
        if self.is_consumed() {
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
        plan.visit_queries_mut(&mut |query| self.apply_query(query));
        // Source selections are consumed before expression rewrites can move their enclosing nodes.
        plan.visit_sources_mut(&mut |source| self.apply_source(source));
        // Children are rewritten before their enclosing clauses move them; every retained identity is consumed at its original address.
        let mut result = Ok(());
        plan.rewrite_scalar_expressions(&mut |expression| {
            if result.is_ok() {
                result = self.apply_node(expression);
            }
        });
        result?;
        self.8.groupings.apply_plan(plan, routines, params)?;
        if !self.2.is_empty() {
            crate::plan::subqueries::rewrite_with_arenas(plan, &mut |node, arena| {
                self.apply_membership(node, arena)
            })?;
        }
        if self.is_consumed() {
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
    }

    fn apply_query(&mut self, query: &mut crate::plan::QueryPlan) {
        install_rewritten_block(&mut self.10, query);
        self.8.apply_query(query);
    }

    fn apply_source(&mut self, source: &mut crate::plan::SourcePlan) {
        // Command inputs have no enclosing QueryPlan; their validated descriptors are not needed by query-output pruning.
        self.8.sources.remove(&NonNull::from(&*source));
        let mut apply = |binding: &mut Option<crate::ast::FunctionBinding>| {
            if let Some(selected) = self.7.remove(&NonNull::from(&*binding)) {
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
        if let Some(analysis) = self.2.remove(&NonNull::from(&*node)) {
            analysis.apply(node, arena)?;
        }
        Ok(())
    }
}
