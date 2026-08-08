//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed positional-row aggregation paths.

use uqa_sql::expr::RowLookup;

use super::{
    estimate_group_bytes, eval_scalar, AdaptiveAggregateSet, CteScope, Engine, SQLError, SQLParam,
    ScalarEvalContext, ScopedEngineHook, Value,
};

struct BorrowedGroupProbe<'a> {
    group_index: &'a super::GroupIndex,
    hash: u64,
    columns: &'a [super::super::projected::ProjectedGroupColumn],
    row: &'a dyn RowLookup,
}

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
            && self.consume_direct_projected(row, params)?
        {
            return Ok(());
        }
        let hook = ScopedEngineHook::new(engine, ctes);
        let context = ScalarEvalContext::from_row_lookup(row, params).with_function_hook(&hook);
        if self.consume_projected_group(row, &context)? {
            return Ok(());
        }
        self.consume_projected_context(row, &context)
    }

    fn consume_direct_projected(
        &mut self,
        row: &dyn RowLookup,
        params: &[SQLParam],
    ) -> Result<bool, SQLError> {
        if self.statement.group_by.is_empty() {
            let hash = self.group_hash(&[])?;
            if !self.observe_direct_key(hash, &[], row, params)? {
                if !self.insert_group(&[], hash)? {
                    return Ok(true);
                }
                if !self.observe_direct_key(hash, &[], row, params)? {
                    return Err(uninitialized_group());
                }
            }
            self.handle_state_overflow()?;
            return Ok(true);
        }
        let Some(columns) = self.projected_group_columns.as_ref() else {
            return Ok(false);
        };
        let hash = super::super::projected::group_hash(columns, row, self.group_index.hasher())
            .map_err(super::super::sort_fallback::exec_to_sql_error)?;
        if !Self::observe_direct_borrowed(
            &mut self.groups,
            BorrowedGroupProbe {
                group_index: &self.group_index,
                hash,
                columns,
                row,
            },
            &self.projected_aggregate_plans,
            self.variable_state,
            &mut self.retained_bytes,
            params,
        )? {
            let null = Value::Null;
            let key = super::super::projected::group_key(columns, row, &null);
            if !self.insert_group(&key, hash)? {
                return Ok(true);
            }
            if !self.observe_direct_key(hash, &key, row, params)? {
                return Err(uninitialized_group());
            }
        }
        self.handle_state_overflow()?;
        Ok(true)
    }

    fn observe_direct_borrowed(
        groups: &mut [super::GroupEntry],
        probe: BorrowedGroupProbe<'_>,
        plans: &super::super::projected_input::ProjectedAggregatePlans,
        variable_state: bool,
        retained_bytes: &mut usize,
        params: &[SQLParam],
    ) -> Result<bool, SQLError> {
        let Some(index) = borrowed_group_index(
            probe.group_index,
            groups,
            probe.hash,
            probe.columns,
            probe.row,
        ) else {
            return Ok(false);
        };
        let entry = &mut groups[index];
        observe_direct_entry(
            plans,
            variable_state,
            retained_bytes,
            entry,
            probe.row,
            params,
        )?;
        Ok(true)
    }

    fn observe_direct_key(
        &mut self,
        hash: u64,
        key: &[Value],
        row: &dyn RowLookup,
        params: &[SQLParam],
    ) -> Result<bool, SQLError> {
        let Some(index) = super::matching_group_index(&self.group_index, &self.groups, hash, key)
        else {
            return Ok(false);
        };
        let entry = &mut self.groups[index];
        observe_direct_entry(
            &self.projected_aggregate_plans,
            self.variable_state,
            &mut self.retained_bytes,
            entry,
            row,
            params,
        )?;
        Ok(true)
    }

    pub(super) fn consume_projected_group(
        &mut self,
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
        let hash = super::super::projected::group_hash(columns, row, self.group_index.hasher())
            .map_err(super::super::sort_fallback::exec_to_sql_error)?;
        if !Self::observe_projected_borrowed(
            &mut self.groups,
            BorrowedGroupProbe {
                group_index: &self.group_index,
                hash,
                columns,
                row,
            },
            &self.projected_aggregate_plans,
            &self.aggregate_targets,
            self.variable_state,
            &mut self.retained_bytes,
            context,
        )? {
            let null = Value::Null;
            let key = super::super::projected::group_key(columns, row, &null);
            if !self.insert_group(&key, hash)? {
                return Ok(true);
            }
            if !self.observe_projected_key(hash, &key, row, context)? {
                return Err(uninitialized_group());
            }
        }
        self.handle_state_overflow()?;
        Ok(true)
    }

    fn observe_projected_borrowed(
        groups: &mut [super::GroupEntry],
        probe: BorrowedGroupProbe<'_>,
        plans: &super::super::projected_input::ProjectedAggregatePlans,
        aggregate_targets: &[uqa_execution::ScalarExpr],
        variable_state: bool,
        retained_bytes: &mut usize,
        context: &ScalarEvalContext<'_>,
    ) -> Result<bool, SQLError> {
        let Some(index) = borrowed_group_index(
            probe.group_index,
            groups,
            probe.hash,
            probe.columns,
            probe.row,
        ) else {
            return Ok(false);
        };
        let entry = &mut groups[index];
        observe_projected_entry(
            plans,
            aggregate_targets,
            variable_state,
            retained_bytes,
            entry,
            probe.row,
            context,
        )?;
        Ok(true)
    }

    fn observe_projected_key(
        &mut self,
        hash: u64,
        key: &[Value],
        row: &dyn RowLookup,
        context: &ScalarEvalContext<'_>,
    ) -> Result<bool, SQLError> {
        let Some(index) = super::matching_group_index(&self.group_index, &self.groups, hash, key)
        else {
            return Ok(false);
        };
        let entry = &mut self.groups[index];
        observe_projected_entry(
            &self.projected_aggregate_plans,
            &self.aggregate_targets,
            self.variable_state,
            &mut self.retained_bytes,
            entry,
            row,
            context,
        )?;
        Ok(true)
    }

    fn consume_projected_context(
        &mut self,
        row: &dyn RowLookup,
        context: &ScalarEvalContext<'_>,
    ) -> Result<(), SQLError> {
        let key = self
            .statement
            .group_by
            .iter()
            .map(|expression| eval_scalar(expression, context))
            .collect::<Result<Vec<_>, _>>()?;
        let hash = self.group_hash(&key)?;
        if !self.observe_projected_key(hash, &key, row, context)? {
            if !self.insert_group(&key, hash)? {
                return Ok(());
            }
            if !self.observe_projected_key(hash, &key, row, context)? {
                return Err(uninitialized_group());
            }
        }
        self.handle_state_overflow()
    }
}

fn borrowed_group_index(
    group_index: &super::GroupIndex,
    groups: &[super::GroupEntry],
    hash: u64,
    columns: &[super::super::projected::ProjectedGroupColumn],
    row: &dyn RowLookup,
) -> Option<usize> {
    group_index.get(&hash).and_then(|bucket| {
        bucket
            .iter()
            .copied()
            .find(|index| super::super::projected::group_matches(columns, &groups[*index].key, row))
    })
}

fn observe_direct_entry(
    plans: &super::super::projected_input::ProjectedAggregatePlans,
    variable_state: bool,
    retained_bytes: &mut usize,
    entry: &mut super::GroupEntry,
    row: &dyn RowLookup,
    params: &[SQLParam],
) -> Result<(), SQLError> {
    let state = &mut entry.state;
    let previous_bytes = state.retained_bytes;
    plans.observe_direct(&mut state.accumulators, row, params)?;
    update_entry_size(variable_state, retained_bytes, entry, previous_bytes)
}

fn observe_projected_entry(
    plans: &super::super::projected_input::ProjectedAggregatePlans,
    aggregate_targets: &[uqa_execution::ScalarExpr],
    variable_state: bool,
    retained_bytes: &mut usize,
    entry: &mut super::GroupEntry,
    row: &dyn RowLookup,
    context: &ScalarEvalContext<'_>,
) -> Result<(), SQLError> {
    let state = &mut entry.state;
    let previous_bytes = state.retained_bytes;
    plans.observe(&mut state.accumulators, aggregate_targets, row, context)?;
    update_entry_size(variable_state, retained_bytes, entry, previous_bytes)
}

fn update_entry_size(
    variable_state: bool,
    retained_bytes: &mut usize,
    entry: &mut super::GroupEntry,
    previous_bytes: usize,
) -> Result<(), SQLError> {
    if variable_state {
        entry.state.retained_bytes = estimate_group_bytes(&entry.key, &entry.state.accumulators);
        *retained_bytes = retained_bytes
            .checked_sub(previous_bytes)
            .and_then(|bytes| bytes.checked_add(entry.state.retained_bytes))
            .ok_or_else(|| SQLError::Internal("aggregate state size overflow".into()))?;
    }
    Ok(())
}

fn uninitialized_group() -> SQLError {
    SQLError::Internal("adaptive aggregate group was not initialized".into())
}
