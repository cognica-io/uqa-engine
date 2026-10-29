//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain selected clause identities and apply unknown-input coercions at their original plan addresses.

use super::{
    AnalysisMap, ColumnType, ConvertedInput, NonNull, OwnedScalarExpr, SQLError, ScalarExpr,
};
use crate::ast::{FunctionBinding, FunctionDispatch};
use crate::catalog::collations::expression::scalar_clause;
use uqa_core::memory::ProductionControl;

struct ClauseInput {
    inner: Option<NonNull<ScalarExpr>>,
    oid: u32,
    retain: bool,
}

#[derive(Default)]
pub(super) struct CollationInputs(AnalysisMap<NonNull<ScalarExpr>, ClauseInput>);

impl CollationInputs {
    pub(super) fn new(control: &uqa_core::memory::ProductionControl<'_>) -> Self {
        Self(AnalysisMap::new(control))
    }
    pub(super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(super) fn select(
        &mut self,
        origin: NonNull<ScalarExpr>,
        inner: Option<NonNull<ScalarExpr>>,
        oid: u32,
    ) -> Result<(), SQLError> {
        self.0.insert_if_missing(origin, || {
            Ok(ClauseInput {
                inner,
                oid,
                retain: true,
            })
        })
    }

    pub(super) fn coerce(
        &mut self,
        mut origin: Option<NonNull<ScalarExpr>>,
        target: &ColumnType,
    ) -> Result<(), SQLError> {
        let mut retain = crate::schema::composites::type_is_collatable(target);
        while let Some(identity) = origin {
            let clause = self.0.get_mut(&identity).ok_or_else(|| {
                SQLError::Internal("input collation did not belong to the analyzed plan".into())
            })?;
            clause.retain = retain;
            retain = false;
            origin = clause.inner;
        }
        Ok(())
    }

    pub(super) fn apply_owned(
        &mut self,
        identity: NonNull<ScalarExpr>,
        expression: OwnedScalarExpr,
        control: &ProductionControl<'_>,
    ) -> Result<OwnedScalarExpr, SQLError> {
        let Some(clause) = self.0.remove(&identity) else {
            return Ok(expression);
        };
        if scalar_clause(&expression)?.is_none() {
            return Err(SQLError::Internal(
                "retained collation no longer matches its syntax".into(),
            ));
        }
        if clause.retain {
            let binding = FunctionBinding::dispatched_with_control(
                FunctionDispatch::Collate {
                    oid: Some(clause.oid),
                },
                control,
            )?;
            Ok(expression.replace_call_binding(Some(binding), control)?)
        } else {
            Ok(expression.select_child(0))
        }
    }

    /// Membership comparisons may select different input types for one unknown literal; each comparison keeps its selected outer clause and value owner.
    pub(super) fn take_literal(
        &self,
        expression: &ScalarExpr,
        literals: &mut AnalysisMap<NonNull<ScalarExpr>, ConvertedInput>,
        control: &ProductionControl<'_>,
    ) -> Result<Option<ConvertedInput>, SQLError> {
        let mut bare = expression;
        while let Some(clause) = scalar_clause(bare)? {
            bare = clause.argument;
        }
        let Some(ConvertedInput {
            expression: literal,
            session_dependent,
        }) = literals.remove(&NonNull::from(bare))
        else {
            return Ok(None);
        };
        let Some(clause) = self
            .0
            .get(&NonNull::from(expression))
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
