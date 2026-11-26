//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain selected clause identities and apply unknown-input coercions at their original plan addresses.

use super::{
    AnalysisIdentity, AnalysisMap, ColumnType, ConvertedInput, OwnedScalarExpr, SQLError,
    ScalarExpr,
};
use crate::ast::{FunctionBinding, FunctionDispatch};
use crate::catalog::collations::{derivation::Derivation, expression::scalar_clause};
use uqa_core::memory::ProductionControl;

struct ClauseInput {
    inner: Option<AnalysisIdentity<ScalarExpr>>,
    oid: u32,
    retain: bool,
    /// The derivation and initial collation before this clause was applied.
    operand: (Derivation, u32),
}

#[derive(Default)]
pub(super) struct CollationInputs(AnalysisMap<AnalysisIdentity<ScalarExpr>, ClauseInput>);

impl CollationInputs {
    pub(super) fn new(control: &uqa_core::memory::ProductionControl<'_>) -> Self {
        Self(AnalysisMap::new(control))
    }
    pub(super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(super) fn select(
        &mut self,
        origin: AnalysisIdentity<ScalarExpr>,
        inner: Option<AnalysisIdentity<ScalarExpr>>,
        oid: u32,
        operand: (Derivation, u32),
    ) -> Result<(), SQLError> {
        self.0.insert_if_missing(origin, || {
            Ok(ClauseInput {
                inner,
                oid,
                retain: true,
                operand,
            })
        })
    }

    pub(super) fn coerce(
        &mut self,
        mut origin: AnalysisIdentity<ScalarExpr>,
        target: &ColumnType,
    ) -> Result<(Derivation, u32), SQLError> {
        let mut retain = crate::schema::composites::type_is_collatable(target);
        loop {
            let clause = self.0.get_mut(&origin).ok_or_else(|| {
                SQLError::Internal("input collation did not belong to the analyzed plan".into())
            })?;
            clause.retain = retain;
            let Some(inner) = clause.inner else {
                return Ok(clause.operand);
            };
            retain = false;
            origin = inner;
        }
    }

    pub(super) async fn apply_owned_from<W: crate::plan::ScalarInputWork>(
        &mut self,
        identity: AnalysisIdentity<ScalarExpr>,
        expression: OwnedScalarExpr,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> Result<OwnedScalarExpr, W::Error>
    where
        W::Error: From<SQLError> + From<crate::schema::retention::CatalogRetentionError>,
    {
        let Some(clause) = self.0.remove(&identity) else {
            return Ok(expression);
        };
        if scalar_clause(&expression)?.is_none() {
            return Err(SQLError::Internal(
                "retained collation no longer matches its syntax".into(),
            )
            .into());
        }
        if clause.retain {
            let binding = FunctionBinding::dispatched_with_control(
                FunctionDispatch::Collate {
                    oid: Some(clause.oid),
                },
                control,
            )?;
            expression
                .replace_call_binding_from(Some(binding), control, work)
                .await
        } else {
            expression.select_child_from(0, control, work).await
        }
    }

    /// Membership comparisons may select different input types for one unknown literal; each comparison keeps its selected outer clause and value owner.
    pub(super) fn take_literal(
        &self,
        expression: &ScalarExpr,
        literals: &mut AnalysisMap<AnalysisIdentity<ScalarExpr>, ConvertedInput>,
        control: &ProductionControl<'_>,
    ) -> Result<Option<ConvertedInput>, SQLError> {
        let mut bare = expression;
        while let Some(clause) = scalar_clause(bare)? {
            bare = clause.argument;
        }
        let Some(ConvertedInput {
            expression: literal,
            session_dependent,
        }) = literals.remove(&AnalysisIdentity::from(bare))
        else {
            return Ok(None);
        };
        let Some(clause) = self
            .0
            .get(&AnalysisIdentity::from(expression))
            .filter(|clause| clause.retain)
        else {
            return Ok(Some(ConvertedInput {
                expression: literal,
                session_dependent,
            }));
        };
        if !matches!(expression, ScalarExpr::Func { .. }) {
            return Ok(None);
        }
        let copied = expression.copy_for_rewrite(control)?;
        let copied = copied.map_child(0, control, |_| Ok::<_, SQLError>(literal))?;
        let binding = FunctionBinding::dispatched_with_control(
            FunctionDispatch::Collate {
                oid: Some(clause.oid),
            },
            control,
        )?;
        Ok(Some(ConvertedInput {
            expression: copied.replace_call_binding(Some(binding), control)?,
            session_dependent,
        }))
    }
}
