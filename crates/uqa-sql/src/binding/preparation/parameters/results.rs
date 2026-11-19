//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sparse result selections from the original analysis, before rewriting inputs.

use super::{
    AnalysisIdentity, AnalysisMap, AnalysisType, ExpressionType, OwnedScalarExpr, SQLError,
    ScalarExpr,
};
use crate::catalog::collations::{derivation::Derivation, expression::scalar_clause};
use crate::ir::ResultCollation;

#[derive(Default)]
pub(super) struct SelectedResults {
    pub(super) enabled: bool,
    selected: AnalysisMap<AnalysisIdentity<ScalarExpr>, (AnalysisType, ResultCollation)>,
}

impl SelectedResults {
    pub(super) fn new(control: &uqa_core::memory::ProductionControl<'_>) -> Self {
        Self {
            enabled: false,
            selected: AnalysisMap::new(control),
        }
    }
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
        let Some(ty) = value.ty.as_deref() else {
            return Ok(());
        };
        // Arithmetic result types can differ from both inputs. Keep the analyzed selection when planning discards a NULL operand and the other input's row scope.
        let arithmetic = matches!(
            expression,
            ScalarExpr::Binary {
                op: crate::ast::BinaryOp::Add
                    | crate::ast::BinaryOp::Subtract
                    | crate::ast::BinaryOp::Multiply
                    | crate::ast::BinaryOp::Divide,
                ..
            }
        );
        if (!arithmetic && !crate::schema::composites::type_is_collatable(ty))
            || (matches!(
                expression,
                ScalarExpr::Literal(_)
                    | ScalarExpr::TypedLiteral { .. }
                    | ScalarExpr::RetainedLiteral(_)
            ) && value.derivation == ExpressionType::default_collation(Some(ty)))
            || matches!(
                value.derivation,
                Derivation::InvalidExplicit(..) | Derivation::InvalidImplicit(..)
            )
        {
            return Ok(());
        }
        self.selected
            .insert_if_missing(AnalysisIdentity::from(expression), || {
                Ok((
                    value.ty.clone().expect("selected result type"),
                    ResultCollation::new(value.derivation, value.initial_collation),
                ))
            })?;
        Ok(())
    }

    pub(super) async fn apply_owned_from<W: crate::plan::ScalarInputWork>(
        &mut self,
        identity: AnalysisIdentity<ScalarExpr>,
        expression: OwnedScalarExpr,
        control: &uqa_core::memory::ProductionControl<'_>,
        work: &mut W,
    ) -> Result<OwnedScalarExpr, W::Error>
    where
        W::Error: From<SQLError> + From<crate::schema::retention::CatalogRetentionError>,
    {
        let Some((ty, collation)) = self.selected.remove(&identity) else {
            return Ok(expression);
        };
        collation.validate_from(&ty, control, work).await?;
        if let (ScalarExpr::TypedLiteral { .. }, Some(oid)) =
            (&*expression, collation.implicit_oid())
        {
            ty.retire_from(control, work).await?;
            Ok(expression.set_literal_collation(oid, control)?)
        } else {
            Ok(OwnedScalarExpr::collated(
                expression,
                ty.into_produced_from(control, work).await?,
                collation,
                control,
            )?)
        }
    }
}
