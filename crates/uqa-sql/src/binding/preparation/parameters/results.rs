//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sparse result selections from the original analysis, before rewriting inputs.

use super::{BTreeMap, ColumnType, ExpressionType, NonNull, SQLError, ScalarExpr};
use crate::catalog::collations::{derivation::Derivation, expression::scalar_clause};
use crate::ir::ResultCollation;

#[derive(Default)]
pub(super) struct CollationResults {
    pub(super) enabled: bool,
    selected: BTreeMap<NonNull<ScalarExpr>, (ColumnType, ResultCollation)>,
}

impl CollationResults {
    pub(super) fn is_empty(&self) -> bool {
        self.selected.is_empty()
    }

    pub(super) fn record(
        &mut self,
        expression: &ScalarExpr,
        value: &ExpressionType,
    ) -> Result<(), SQLError> {
        if !self.enabled
            || matches!(
                expression,
                ScalarExpr::Collated { .. }
                    | ScalarExpr::Column(_)
                    | ScalarExpr::QualifiedColumn { .. }
                    | ScalarExpr::Position(_)
                    | ScalarExpr::InternalColumn(_)
                    | ScalarExpr::Param(_)
            )
            || scalar_clause(expression)?.is_some()
            || matches!(expression, ScalarExpr::Func { binding: Some(binding), .. } if binding.dispatch.is_some_and(crate::ast::FunctionDispatch::is_call_argument_marker))
        {
            return Ok(());
        }
        let Some(ty) = value.ty.as_ref() else {
            return Ok(());
        };
        if !crate::schema::composites::type_is_collatable(ty)
            || (matches!(
                expression,
                ScalarExpr::Literal(_) | ScalarExpr::TypedLiteral { .. }
            ) && value.derivation == ExpressionType::default_collation(Some(ty)))
            || matches!(
                value.derivation,
                Derivation::InvalidExplicit(..) | Derivation::InvalidImplicit(..)
            )
        {
            return Ok(());
        }
        self.selected
            .entry(NonNull::from(expression))
            .or_insert_with(|| {
                (
                    ty.clone(),
                    ResultCollation::new(value.derivation, value.initial_collation),
                )
            });
        Ok(())
    }

    pub(super) fn apply(
        &mut self,
        identity: NonNull<ScalarExpr>,
        expression: &mut ScalarExpr,
    ) -> Result<(), SQLError> {
        let Some((ty, collation)) = self.selected.remove(&identity) else {
            return Ok(());
        };
        collation.validate(&ty)?;
        if let (ScalarExpr::TypedLiteral { collation_oid, .. }, Some(oid)) =
            (&mut *expression, collation.implicit_oid())
        {
            *collation_oid = Some(oid);
        } else {
            let inner = std::mem::replace(expression, ScalarExpr::Literal(uqa_core::Value::Null));
            *expression = ScalarExpr::Collated {
                expr: Box::new(inner),
                ty,
                collation,
            };
        }
        Ok(())
    }
}
