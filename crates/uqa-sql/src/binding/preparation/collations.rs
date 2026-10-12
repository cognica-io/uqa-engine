//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Report collation conflicts only after the containing query has completed type analysis.

use crate::catalog::collations::derivation::Derivation;
use crate::plan::{CommandPlan, QueryPlan, RelationalPlan};
use crate::{SQLError, ScalarExpr};
use std::{collections::BTreeMap, ptr::NonNull};

/// Valid inputs allocate no diagnostic storage. Addresses identify borrowed syntax and are never dereferenced or retained in an executable plan.
#[derive(Default)]
pub(super) struct PendingCollations(BTreeMap<NonNull<ScalarExpr>, Derivation>);

impl PendingCollations {
    pub(super) fn record(&mut self, expression: &ScalarExpr, selected: Derivation) {
        if matches!(selected, Derivation::InvalidExplicit(..)) {
            self.0.insert(NonNull::from(expression), selected);
        }
    }

    pub(super) fn validate_expression(&self, expression: &ScalarExpr) -> Result<(), SQLError> {
        if self.0.is_empty() {
            return Ok(());
        }
        let mut result = Ok(());
        expression.visit(&mut |node| {
            if result.is_ok() {
                if let Some(selected) = self.0.get(&NonNull::from(node)) {
                    result = selected.validate();
                }
            }
        });
        result
    }

    pub(super) fn validate_query(&self, query: &QueryPlan) -> Result<(), SQLError> {
        if self.0.is_empty() {
            return Ok(());
        }
        if let RelationalPlan::QueryBlock(block) = &query.root {
            // PostgreSQL assigns the target list (including clause-added target entries) before the join tree and its qualifications.
            for expression in block
                .projections
                .iter()
                .map(|p| &p.expr)
                .chain(block.order_by.iter().map(|o| &o.expr))
                .chain(block.group_by.iter())
                .chain(block.grouping_sets.iter().flatten())
                .chain(block.distinct_on.iter())
            {
                self.validate_expression(expression)?;
            }
            for window in &block.windows {
                for expression in window.spec.expressions() {
                    self.validate_expression(expression)?;
                }
            }
            if let Some(source) = &block.from {
                let mut result = Ok(());
                source.visit_scalar_expressions(&mut |expression| {
                    if result.is_ok() {
                        result = self.validate_expression(expression);
                    }
                });
                result?;
            }
            for expression in block
                .r#where
                .iter()
                .chain(block.having.iter())
                .chain(block.offset.iter())
                .chain(block.limit.iter())
            {
                self.validate_expression(expression)?;
            }
            return Ok(());
        }
        let mut result = Ok(());
        query.visit_scalar_expressions(&mut |expression| {
            if result.is_ok() {
                result = self.validate_expression(expression);
            }
        });
        result
    }

    pub(super) fn validate_command(&self, command: &CommandPlan) -> Result<(), SQLError> {
        if self.0.is_empty() {
            return Ok(());
        }
        let mut result = Ok(());
        command.visit_scalar_expressions(&mut |expression| {
            if result.is_ok() {
                result = self.validate_expression(expression);
            }
        });
        result
    }
}
