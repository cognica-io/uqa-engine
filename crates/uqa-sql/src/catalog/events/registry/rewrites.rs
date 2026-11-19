//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rewrite stored composite values without mutating any retained event generation.

use super::{EventCatalog, StoredRule, StoredTrigger};
use crate::{expr::composites::constants::CompositeConstantChange, SQLError};

impl EventCatalog<StoredTrigger> {
    pub fn rewrite_composite_constants(
        &mut self,
        change: &CompositeConstantChange<'_>,
    ) -> Result<bool, SQLError> {
        self.rewrite(|_, _, original| {
            let Some(expression) = &original.definition.when else {
                return Ok(None);
            };
            let schema = change
                .rename
                .map_or(Ok(crate::RowSchema::default()), |rename| {
                    rename.transition_schema(&original.definition.table)
                })?;
            let mut expression = expression.clone();
            if !change.expression_in_schema(&mut expression, &schema)? {
                return Ok(None);
            }
            let mut trigger = original.clone();
            trigger.definition.when = Some(expression);
            Ok(Some(trigger))
        })
    }
}

impl EventCatalog<StoredRule> {
    pub fn rewrite_composite_constants(
        &mut self,
        change: &CompositeConstantChange<'_>,
    ) -> Result<bool, SQLError> {
        self.rewrite(|_, _, original| {
            let mut rule = original.clone();
            let schema = change
                .rename
                .map_or(Ok(crate::RowSchema::default()), |rename| {
                    rename.transition_schema(&rule.definition.table)
                })?;
            let mut changed = false;
            if let Some(expression) = &mut rule.definition.condition {
                changed |= change.expression_in_schema(expression, &schema)?;
            }
            if let Some(plan) = &mut rule.condition_plan {
                if let Some(rename) = change.rename {
                    changed |= rename.expression_plan(plan, &schema)?;
                }
                changed |= change.expression_plan(plan)?;
            }
            for statement in &mut rule.definition.actions {
                changed |= change.statement_in_scope(statement, None, Some(&schema))?;
            }
            Ok(changed.then_some(rule))
        })
    }
}
