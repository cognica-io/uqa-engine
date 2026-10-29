//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain input collations and type-observer selections on already bound calls without rereading constants or changing routine identities.

use super::{AnalysisMap, NonNull, SQLError, ScalarExpr};
use crate::ast::FunctionBinding;
use crate::catalog::collations::CollationSelection;
use uqa_core::memory::{Produced, ProductionControl};

/// The selections are applied at the analyzed expression's original addresses; nothing else rewrites the expression between analysis and application.
#[derive(Default)]
pub(in crate::binding::preparation) struct CallCollations {
    selections: AnalysisMap<NonNull<ScalarExpr>, Option<CollationSelection>>,
    builtins: AnalysisMap<NonNull<ScalarExpr>, Produced<FunctionBinding>>,
    observers: AnalysisMap<NonNull<ScalarExpr>, crate::ast::FunctionDispatch>,
}

impl CallCollations {
    pub(in crate::binding::preparation) fn new(control: &ProductionControl<'_>) -> Self {
        Self {
            selections: AnalysisMap::new(control),
            builtins: AnalysisMap::new(control),
            observers: AnalysisMap::new(control),
        }
    }
    pub(in crate::binding::preparation) fn record_observer(
        &mut self,
        expression: &ScalarExpr,
        selected: &FunctionBinding,
    ) -> Result<(), SQLError> {
        if let Some(
            dispatch @ (crate::ast::FunctionDispatch::CollationFor { .. }
            | crate::ast::FunctionDispatch::TypeOf { .. }),
        ) = selected.dispatch
        {
            self.observers.insert(NonNull::from(expression), dispatch)?;
        }
        Ok(())
    }

    pub(in crate::binding::preparation) fn record(
        &mut self,
        expression: &ScalarExpr,
        selection: Option<CollationSelection>,
    ) -> Result<(), SQLError> {
        self.selections
            .insert(NonNull::from(expression), selection)?;
        Ok(())
    }

    pub(in crate::binding::preparation) fn record_builtin(
        &mut self,
        expression: &ScalarExpr,
        selected: Produced<FunctionBinding>,
    ) -> Result<(), SQLError> {
        self.builtins.insert(NonNull::from(expression), selected)?;
        Ok(())
    }

    pub(in crate::binding::preparation) fn apply(
        mut self,
        expression: &mut ScalarExpr,
        subqueries: &mut [crate::plan::QueryPlan],
    ) -> Result<(), SQLError> {
        let mut apply = |node: &mut ScalarExpr| self.apply_node(node);
        crate::plan::rewrite_scalar_expression(expression, &mut apply);
        for query in subqueries.iter_mut() {
            query.rewrite_scalar_expressions(&mut apply);
        }
        self.finish()?;
        for query in subqueries {
            query.normalize_window_definitions()?;
        }
        Ok(())
    }

    pub(in crate::binding::preparation) fn apply_query(
        mut self,
        query: &mut crate::plan::QueryPlan,
    ) -> Result<(), SQLError> {
        query.rewrite_scalar_expressions(&mut |node| self.apply_node(node));
        self.finish()?;
        query.normalize_window_definitions()
    }

    fn apply_node(&mut self, node: &mut ScalarExpr) {
        let identity = NonNull::from(&*node);
        let ScalarExpr::Func { binding, .. } = node else {
            return;
        };
        if let Some(selected) = self.builtins.remove(&identity) {
            if binding.is_none() {
                *binding = Some(
                    selected
                        .into_uncontrolled()
                        .expect("ordinary stored call application"),
                );
            }
        }
        if let (Some(selection), Some(binding)) =
            (self.selections.remove(&identity), binding.as_mut())
        {
            binding.input_collation = selection;
        }
        if let (Some(dispatch), Some(binding)) =
            (self.observers.remove(&identity), binding.as_mut())
        {
            binding.dispatch = Some(dispatch);
        }
    }

    fn finish(&self) -> Result<(), SQLError> {
        if self.selections.is_empty() && self.builtins.is_empty() && self.observers.is_empty() {
            Ok(())
        } else {
            Err(SQLError::Internal(
                "input collation did not belong to the analyzed expression".into(),
            ))
        }
    }
}
