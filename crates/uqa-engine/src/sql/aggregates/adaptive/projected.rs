//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed positional-row aggregation paths.

use uqa_sql::expr::RowLookup;

use super::{
    estimate_group_bytes, eval_scalar, AdaptiveAggregateSet, CteScope, Engine, SQLError, SQLParam,
    ScalarEvalContext, ScopedEngineHook, Value, PROJECTED_GROUP_LINEAR_LOOKUP_LIMIT,
};

impl AdaptiveAggregateSet {
    pub(in crate::sql::aggregates) fn consume_projected_row(
        &mut self,
        engine: &Engine,
        row: &dyn RowLookup,
        params: &[SQLParam],
        ctes: &CteScope,
    ) -> Result<(), SQLError> {
        debug_assert!(self.statement.subqueries.is_empty());
        if self.projected_aggregate_plans.all_direct()
            && self.consume_direct_projected(engine, row, params)?
        {
            return Ok(());
        }
        let hook = ScopedEngineHook::new(engine, ctes);
        let context = ScalarEvalContext::from_row_lookup(row, params).with_function_hook(&hook);
        if self.consume_projected_group(engine, row, &context)? {
            return Ok(());
        }
        self.consume_projected_context(engine, row, &context)
    }

    fn consume_direct_projected(
        &mut self,
        engine: &Engine,
        row: &dyn RowLookup,
        params: &[SQLParam],
    ) -> Result<bool, SQLError> {
        if self.statement.group_by.is_empty() {
            if self.groups.is_empty() {
                self.ensure_group(engine, &[])?;
            }
            self.observe_direct_projected_state(row, &[], params)?;
            return Ok(true);
        }
        let Some(columns) = self.projected_group_columns.as_ref() else {
            return Ok(false);
        };
        if self.groups.len() > PROJECTED_GROUP_LINEAR_LOOKUP_LIMIT {
            return Ok(false);
        }

        let null = Value::Null;
        let fingerprint = super::super::projected::group_fingerprint(columns, row, &null);
        if let Some((key, state)) = self.groups.iter_mut().find(|(key, state)| {
            state.projected_fingerprint == Some(fingerprint)
                && super::super::projected::group_matches(columns, key, row, &null)
        }) {
            let previous_bytes = state.retained_bytes;
            self.projected_aggregate_plans
                .observe_direct(&mut state.accumulators, row, params)?;
            if self.variable_state {
                state.retained_bytes = estimate_group_bytes(key, &state.accumulators);
                self.retained_bytes = self
                    .retained_bytes
                    .checked_sub(previous_bytes)
                    .and_then(|bytes| bytes.checked_add(state.retained_bytes))
                    .ok_or_else(|| SQLError::Internal("aggregate state size overflow".into()))?;
            }
            if self.retained_bytes > self.state_budget {
                self.flush_partial_groups()?;
            }
            return Ok(true);
        }

        let key = super::super::projected::group_key(columns, row, &null);
        self.ensure_group(engine, &key)?;
        let state = self.groups.get_mut(&key).ok_or_else(|| {
            SQLError::Internal("adaptive aggregate group was not initialized".into())
        })?;
        state.projected_fingerprint = Some(fingerprint);
        self.observe_direct_projected_state(row, &key, params)?;
        Ok(true)
    }

    fn observe_direct_projected_state(
        &mut self,
        row: &dyn RowLookup,
        key: &[Value],
        params: &[SQLParam],
    ) -> Result<(), SQLError> {
        let state = self.groups.get_mut(key).ok_or_else(|| {
            SQLError::Internal("adaptive aggregate group was not initialized".into())
        })?;
        let previous_bytes = state.retained_bytes;
        self.projected_aggregate_plans
            .observe_direct(&mut state.accumulators, row, params)?;
        self.update_projected_state_size(key, previous_bytes)?;
        Ok(())
    }

    fn consume_projected_group(
        &mut self,
        engine: &Engine,
        row: &dyn RowLookup,
        context: &ScalarEvalContext<'_>,
    ) -> Result<bool, SQLError> {
        let Some(columns) = self
            .projected_group_columns
            .as_ref()
            .filter(|columns| !columns.is_empty())
        else {
            return Ok(false);
        };
        if self.groups.len() > PROJECTED_GROUP_LINEAR_LOOKUP_LIMIT {
            return Ok(false);
        }

        let null = Value::Null;
        let fingerprint = super::super::projected::group_fingerprint(columns, row, &null);
        let existing = self.groups.iter_mut().find(|(key, state)| {
            state.projected_fingerprint == Some(fingerprint)
                && super::super::projected::group_matches(columns, key, row, &null)
        });
        let Some((key, state)) = existing else {
            let key = super::super::projected::group_key(columns, row, &null);
            self.ensure_group(engine, &key)?;
            let state = self.groups.get_mut(&key).ok_or_else(|| {
                SQLError::Internal("adaptive aggregate group was not initialized".into())
            })?;
            state.projected_fingerprint = Some(fingerprint);
            self.observe_projected_state(row, &key, context)?;
            return Ok(true);
        };

        let previous_bytes = state.retained_bytes;
        self.projected_aggregate_plans.observe(
            &mut state.accumulators,
            &self.aggregate_targets,
            row,
            context,
        )?;
        if self.variable_state {
            state.retained_bytes = estimate_group_bytes(key, &state.accumulators);
            self.retained_bytes = self
                .retained_bytes
                .checked_sub(previous_bytes)
                .and_then(|bytes| bytes.checked_add(state.retained_bytes))
                .ok_or_else(|| SQLError::Internal("aggregate state size overflow".into()))?;
        }
        if self.retained_bytes > self.state_budget {
            self.flush_partial_groups()?;
        }
        Ok(true)
    }

    fn observe_projected_state(
        &mut self,
        row: &dyn RowLookup,
        key: &[Value],
        context: &ScalarEvalContext<'_>,
    ) -> Result<(), SQLError> {
        let state = self.groups.get_mut(key).ok_or_else(|| {
            SQLError::Internal("adaptive aggregate group was not initialized".into())
        })?;
        let previous_bytes = state.retained_bytes;
        self.projected_aggregate_plans.observe(
            &mut state.accumulators,
            &self.aggregate_targets,
            row,
            context,
        )?;
        self.update_projected_state_size(key, previous_bytes)
    }

    fn update_projected_state_size(
        &mut self,
        key: &[Value],
        previous_bytes: usize,
    ) -> Result<(), SQLError> {
        if self.variable_state {
            let state = self.groups.get_mut(key).ok_or_else(|| {
                SQLError::Internal("adaptive aggregate group was not initialized".into())
            })?;
            state.retained_bytes = estimate_group_bytes(key, &state.accumulators);
            self.retained_bytes = self
                .retained_bytes
                .checked_sub(previous_bytes)
                .and_then(|bytes| bytes.checked_add(state.retained_bytes))
                .ok_or_else(|| SQLError::Internal("aggregate state size overflow".into()))?;
        }
        if !self.statement.group_by.is_empty() && self.retained_bytes > self.state_budget {
            self.flush_partial_groups()?;
        }
        Ok(())
    }

    fn consume_projected_context(
        &mut self,
        engine: &Engine,
        row: &dyn RowLookup,
        context: &ScalarEvalContext<'_>,
    ) -> Result<(), SQLError> {
        let key = self
            .statement
            .group_by
            .iter()
            .map(|expression| eval_scalar(expression, context))
            .collect::<Result<Vec<_>, _>>()?;
        self.ensure_group(engine, &key)?;
        self.observe_projected_state(row, &key, context)
    }
}
