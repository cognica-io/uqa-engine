//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Input collations recorded on the calls of an expression whose routines are already bound, without any other analysis effect on the expression.

use super::{BTreeMap, NonNull, SQLError, ScalarExpr};
use crate::ast::FunctionBinding;
use crate::catalog::collations::CollationSelection;

/// The selections are applied at the analyzed expression's original addresses; nothing else rewrites the expression between analysis and application.
#[derive(Default)]
pub(in crate::binding::preparation) struct CallCollations {
    selections: BTreeMap<NonNull<ScalarExpr>, Option<CollationSelection>>,
    builtins: BTreeMap<NonNull<ScalarExpr>, FunctionBinding>,
}

impl CallCollations {
    pub(in crate::binding::preparation) fn record(
        &mut self,
        expression: &ScalarExpr,
        selection: Option<CollationSelection>,
    ) {
        self.selections.insert(NonNull::from(expression), selection);
    }

    pub(in crate::binding::preparation) fn record_builtin(
        &mut self,
        expression: &ScalarExpr,
        selected: FunctionBinding,
    ) {
        self.builtins.insert(NonNull::from(expression), selected);
    }

    pub(in crate::binding::preparation) fn apply(
        mut self,
        expression: &mut ScalarExpr,
        subqueries: &mut [crate::plan::QueryPlan],
    ) -> Result<(), SQLError> {
        let mut apply = |node: &mut ScalarExpr| {
            let identity = NonNull::from(&*node);
            let ScalarExpr::Func { binding, .. } = node else {
                return;
            };
            if let Some(selected) = self.builtins.remove(&identity) {
                if binding.is_none() {
                    *binding = Some(selected);
                }
            }
            if let (Some(selection), Some(binding)) =
                (self.selections.remove(&identity), binding.as_mut())
            {
                binding.input_collation = selection;
            }
        };
        crate::plan::rewrite_scalar_expression(expression, &mut apply);
        for query in subqueries.iter_mut() {
            query.rewrite_scalar_expressions(&mut apply);
        }
        if self.selections.is_empty() && self.builtins.is_empty() {
            Ok(())
        } else {
            Err(SQLError::Internal(
                "input collation did not belong to the analyzed expression".into(),
            ))
        }
    }
}
