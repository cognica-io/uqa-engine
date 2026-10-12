//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain selected clause identities and apply unknown-input coercions at their original plan addresses.

use super::{BTreeMap, ColumnType, NonNull, SQLError, ScalarExpr};
use crate::ast::{FunctionBinding, FunctionDispatch};
use crate::catalog::collations::expression::{discard_scalar_clause, scalar_clause};

struct ClauseInput {
    inner: Option<NonNull<ScalarExpr>>,
    oid: u32,
    retain: bool,
}

#[derive(Default)]
pub(super) struct CollationInputs(BTreeMap<NonNull<ScalarExpr>, ClauseInput>);

impl CollationInputs {
    pub(super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(super) fn select(
        &mut self,
        origin: NonNull<ScalarExpr>,
        inner: Option<NonNull<ScalarExpr>>,
        oid: u32,
    ) {
        self.0.entry(origin).or_insert(ClauseInput {
            inner,
            oid,
            retain: true,
        });
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

    pub(super) fn apply(
        &mut self,
        identity: NonNull<ScalarExpr>,
        expression: &mut ScalarExpr,
    ) -> Result<(), SQLError> {
        let Some(clause) = self.0.remove(&identity) else {
            return Ok(());
        };
        if scalar_clause(expression)?.is_none() {
            return Err(SQLError::Internal(
                "retained collation no longer matches its syntax".into(),
            ));
        }
        if clause.retain {
            let ScalarExpr::Func { binding, .. } = expression else {
                unreachable!("validated collation clause");
            };
            *binding = Some(FunctionBinding::dispatched(FunctionDispatch::Collate {
                oid: Some(clause.oid),
            }));
        } else {
            discard_scalar_clause(expression)?;
        }
        Ok(())
    }

    /// Membership comparisons can select different input types for the same unknown literal. Capture each converted value with its selected outer clause before the next comparison reads that input again.
    pub(super) fn take_literal(
        &self,
        expression: &ScalarExpr,
        literals: &mut BTreeMap<NonNull<ScalarExpr>, ScalarExpr>,
    ) -> Option<ScalarExpr> {
        let mut bare = expression;
        while let Some(clause) = scalar_clause(bare).ok()? {
            bare = clause.argument;
        }
        let literal = literals.remove(&NonNull::from(bare))?;
        let Some(clause) = self
            .0
            .get(&NonNull::from(expression))
            .filter(|clause| clause.retain)
        else {
            return Some(literal);
        };
        let ScalarExpr::Func {
            name,
            args,
            distinct,
            order_by,
            order_syntax,
            filter,
            ..
        } = expression
        else {
            return None;
        };
        Some(ScalarExpr::Func {
            name: name.clone(),
            binding: Some(FunctionBinding::dispatched(FunctionDispatch::Collate {
                oid: Some(clause.oid),
            })),
            args: vec![literal, args[1].clone()],
            distinct: *distinct,
            order_by: order_by.clone(),
            order_syntax: *order_syntax,
            filter: filter.clone(),
        })
    }
}
