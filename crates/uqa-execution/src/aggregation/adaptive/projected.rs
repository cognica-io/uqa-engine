//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed positional-row aggregation paths.

use crate::ProjectedRow;
use uqa_sql::expr::RowLookup;

use super::{
    estimate_group_bytes, eval_scalar, AdaptiveAggregateSet, QueryExpressionContext, SQLError,
    SQLParam, ScalarEvalContext, Value,
};

struct BorrowedGroupProbe<'probe, Row> {
    group_index: &'probe super::GroupIndex,
    hash: u64,
    columns: &'probe [super::super::projected::ProjectedGroupColumn],
    row: &'probe Row,
    enums: Option<&'probe dyn uqa_sql::expr::enums::EnumLabelCatalog>,
}

impl AdaptiveAggregateSet {
    pub(in crate::aggregation) fn consume_projected_row(
        &mut self,
        context: &dyn QueryExpressionContext,
        row: &ProjectedRow<'_, '_>,
        params: &[SQLParam],
    ) -> Result<(), SQLError> {
        debug_assert!(self.statement.subqueries.is_empty());
        if self.projected_aggregate_plans.all_direct()
            && self.consume_direct_projected(
                row,
                params,
                uqa_sql::expr::EngineHook::enum_labels(context),
            )?
        {
            return Ok(());
        }
        let hook = context;
        let context = ScalarEvalContext::from_row_lookup(row, params)
            .with_row_schema(row.row_schema())
            .with_function_hook(hook);
        if self.consume_projected_group(row, &context)? {
            return Ok(());
        }
        self.consume_projected_context(row, &context)
    }

    fn consume_direct_projected(
        &mut self,
        row: &ProjectedRow<'_, '_>,
        params: &[SQLParam],
        enums: Option<&dyn uqa_sql::expr::enums::EnumLabelCatalog>,
    ) -> Result<bool, SQLError> {
        if self.statement.group_by.is_empty() {
            if self.groups.len() == 1 {
                observe_direct_entry(
                    &self.projected_aggregate_plans,
                    self.variable_state,
                    &mut self.retained_bytes,
                    &mut self.groups[0],
                    row,
                    params,
                    enums,
                )?;
                return Ok(true);
            }
            if self.groups.len() > 1 {
                return Err(SQLError::Internal(
                    "ungrouped aggregate retained more than one group".into(),
                ));
            }
            let hash = self.group_hash(&[], None)?;
            if !self.observe_direct_key(hash, &[], row, params, enums)? {
                if !self.insert_group(&[], hash)? {
                    return Ok(true);
                }
                if !self.observe_direct_key(hash, &[], row, params, enums)? {
                    return Err(uninitialized_group());
                }
            }
            return Ok(true);
        }
        if self.projected_group_columns.is_none() {
            return Ok(false);
        }
        let compact_key = super::super::projected::compact_text_pair(
            self.projected_group_columns
                .as_ref()
                .expect("projected group columns disappeared"),
            row,
        );
        if let Some(compact_key) = compact_key {
            if !self.observe_direct_compact_text(compact_key, row, params, enums)? {
                let null = Value::Null;
                let key = super::super::projected::group_key(
                    self.projected_group_columns
                        .as_ref()
                        .expect("projected group columns disappeared"),
                    row,
                    &null,
                );
                if !self.insert_group(&key, compact_key)? {
                    return Ok(true);
                }
                if !self.observe_direct_compact_text(compact_key, row, params, enums)? {
                    return Err(uninitialized_group());
                }
            }
            self.handle_state_overflow()?;
            return Ok(true);
        }
        let columns = self
            .projected_group_columns
            .as_ref()
            .expect("projected group columns disappeared");
        let hash =
            super::super::projected::group_hash(columns, row, self.group_index.hasher(), enums)
                .map_err(super::super::sort_fallback::exec_to_sql_error)?;
        let observed = Self::observe_direct_borrowed(
            &mut self.groups,
            BorrowedGroupProbe {
                group_index: &self.group_index,
                hash,
                columns,
                row,
                enums,
            },
            &self.projected_aggregate_plans,
            self.variable_state,
            &mut self.retained_bytes,
            params,
        )?;
        if !observed {
            let null = Value::Null;
            let key = super::super::projected::group_key(columns, row, &null);
            if !self.insert_group(&key, hash)? {
                return Ok(true);
            }
            if !self.observe_direct_key(hash, &key, row, params, enums)? {
                return Err(uninitialized_group());
            }
        }
        self.handle_state_overflow()?;
        Ok(true)
    }

    fn observe_direct_compact_text(
        &mut self,
        key: u64,
        row: &ProjectedRow<'_, '_>,
        params: &[SQLParam],
        enums: Option<&dyn uqa_sql::expr::enums::EnumLabelCatalog>,
    ) -> Result<bool, SQLError> {
        let index = self.compact_text_group_index.as_ref().ok_or_else(|| {
            SQLError::Internal("compact text aggregate group index is unavailable".into())
        })?;
        let Some(&index) = index.get(&key) else {
            return Ok(false);
        };
        let entry = self.groups.get_mut(index).ok_or_else(|| {
            SQLError::Internal("compact text aggregate group index is stale".into())
        })?;
        observe_direct_entry(
            &self.projected_aggregate_plans,
            self.variable_state,
            &mut self.retained_bytes,
            entry,
            row,
            params,
            enums,
        )?;
        Ok(true)
    }

    fn observe_direct_borrowed(
        groups: &mut [super::GroupEntry],
        probe: BorrowedGroupProbe<'_, ProjectedRow<'_, '_>>,
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
            probe.enums,
        )?
        else {
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
            probe.enums,
        )?;
        Ok(true)
    }

    fn observe_direct_key(
        &mut self,
        hash: u64,
        key: &[Value],
        row: &ProjectedRow<'_, '_>,
        params: &[SQLParam],
        enums: Option<&dyn uqa_sql::expr::enums::EnumLabelCatalog>,
    ) -> Result<bool, SQLError> {
        let Some(index) =
            super::matching_group_index(&self.group_index, &self.groups, hash, key, enums)?
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
            enums,
        )?;
        Ok(true)
    }

    pub(super) fn consume_projected_group<Row: RowLookup>(
        &mut self,
        row: &Row,
        context: &ScalarEvalContext<'_>,
    ) -> Result<bool, SQLError> {
        let Some(columns) = self
            .projected_group_columns
            .as_ref()
            .filter(|columns| !columns.is_empty())
        else {
            return Ok(false);
        };
        let enums = context
            .function_hook()
            .and_then(uqa_sql::expr::EngineHook::enum_labels);
        let hash =
            super::super::projected::group_hash(columns, row, self.group_index.hasher(), enums)
                .map_err(super::super::sort_fallback::exec_to_sql_error)?;
        if !Self::observe_projected_borrowed(
            &mut self.groups,
            BorrowedGroupProbe {
                group_index: &self.group_index,
                hash,
                columns,
                row,
                enums,
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

    fn observe_projected_borrowed<Row: RowLookup>(
        groups: &mut [super::GroupEntry],
        probe: BorrowedGroupProbe<'_, Row>,
        plans: &super::super::projected_input::ProjectedAggregatePlans,
        aggregate_targets: &crate::scalar::PreparedExpressions<Vec<crate::ScalarExpr>>,
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
            probe.enums,
        )?
        else {
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
            &(*context).with_function_states(aggregate_targets.calls()),
        )?;
        Ok(true)
    }

    fn observe_projected_key<Row: RowLookup>(
        &mut self,
        hash: u64,
        key: &[Value],
        row: &Row,
        context: &ScalarEvalContext<'_>,
    ) -> Result<bool, SQLError> {
        let Some(index) = super::matching_group_index(
            &self.group_index,
            &self.groups,
            hash,
            key,
            context
                .function_hook()
                .and_then(uqa_sql::expr::EngineHook::enum_labels),
        )?
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
            &(*context).with_function_states(self.aggregate_targets.calls()),
        )?;
        Ok(true)
    }

    fn consume_projected_context<Row: RowLookup>(
        &mut self,
        row: &Row,
        context: &ScalarEvalContext<'_>,
    ) -> Result<(), SQLError> {
        let group_context = (*context).with_function_states(self.group_expressions.calls());
        let key = self
            .group_expressions
            .iter()
            .map(|expression| eval_scalar(expression, &group_context))
            .collect::<Result<Vec<_>, _>>()?;
        let hash = self.group_hash(
            &key,
            context
                .function_hook()
                .and_then(uqa_sql::expr::EngineHook::enum_labels),
        )?;
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

fn borrowed_group_index<Row: RowLookup>(
    group_index: &super::GroupIndex,
    groups: &[super::GroupEntry],
    hash: u64,
    columns: &[super::super::projected::ProjectedGroupColumn],
    row: &Row,
    enums: Option<&dyn uqa_sql::expr::enums::EnumLabelCatalog>,
) -> Result<Option<usize>, SQLError> {
    let Some(bucket) = group_index.get(&hash) else {
        return Ok(None);
    };
    for index in bucket {
        if super::super::projected::group_matches(columns, &groups[*index].key, row, enums)? {
            return Ok(Some(*index));
        }
    }
    Ok(None)
}

fn observe_direct_entry(
    plans: &super::super::projected_input::ProjectedAggregatePlans,
    variable_state: bool,
    retained_bytes: &mut usize,
    entry: &mut super::GroupEntry,
    row: &ProjectedRow<'_, '_>,
    params: &[SQLParam],
    enums: Option<&dyn uqa_sql::expr::enums::EnumLabelCatalog>,
) -> Result<(), SQLError> {
    let state = &mut entry.state;
    let previous_bytes = state.retained_bytes;
    plans.observe_direct(&mut state.accumulators, row, params, enums)?;
    update_entry_size(variable_state, retained_bytes, entry, previous_bytes)
}

fn observe_projected_entry<Row: RowLookup>(
    plans: &super::super::projected_input::ProjectedAggregatePlans,
    aggregate_targets: &[crate::ScalarExpr],
    variable_state: bool,
    retained_bytes: &mut usize,
    entry: &mut super::GroupEntry,
    row: &Row,
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
