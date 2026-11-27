//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed positional-row aggregation paths.

use crate::ProjectedRow;
use uqa_sql::expr::RowLookup;

use super::{
    AdaptiveAggregateSet, QueryExpressionContext, SQLError, SQLParam, ScalarEvalContext, Value,
};

impl AdaptiveAggregateSet {
    pub(in crate::aggregation) fn consume_projected_row(
        &mut self,
        context: &dyn QueryExpressionContext,
        row: &ProjectedRow<'_, '_>,
        params: &[SQLParam],
    ) -> Result<(), SQLError> {
        debug_assert!(self.subqueries.is_empty());
        let scalar = ScalarEvalContext::from_row_lookup(row, params)
            .with_row_schema(row.row_schema())
            .with_function_hook(context);
        let Some(index) = self.group_for_row(row, &scalar)? else {
            return Ok(());
        };
        if self.projected_aggregate_plans.all_direct() && self.projected_group_columns.is_some() {
            self.projected_aggregate_plans.observe_direct(
                &mut self.groups[index].state.accumulators,
                row,
                params,
                uqa_sql::expr::EngineHook::enum_labels(context),
            )?;
        } else {
            self.observe_projected(index, row, &scalar)?;
        }
        self.finish_observation(index)
    }

    pub(super) fn group_for_row<Row: RowLookup>(
        &mut self,
        row: &Row,
        context: &ScalarEvalContext<'_>,
    ) -> Result<Option<usize>, SQLError> {
        if self.group_by.is_empty() {
            return self.ungrouped_index();
        }
        let Some(columns) = self.projected_group_columns.as_deref() else {
            return self.evaluated_group_index(context);
        };
        let enums = context
            .function_hook()
            .and_then(uqa_sql::expr::EngineHook::enum_labels);
        let (hash, index) = if let Some(key) =
            super::super::projected::compact_text_pair(columns, row)
        {
            let index = self
                .compact_text_group_index
                .as_ref()
                .and_then(|index| index.get(&key).copied());
            (key, index)
        } else {
            let hash =
                super::super::projected::group_hash(columns, row, self.group_index.hasher(), enums)
                    .map_err(super::super::sort_fallback::exec_to_sql_error)?;
            let index =
                borrowed_group_index(&self.group_index, &self.groups, hash, columns, row, enums)?;
            (hash, index)
        };
        if let Some(index) = index {
            return Ok(Some(index));
        }
        let key = super::super::projected::group_key(columns, row, &Value::Null);
        self.insert_group(key, hash)
    }

    pub(super) fn observe_projected<Row: RowLookup>(
        &mut self,
        index: usize,
        row: &Row,
        context: &ScalarEvalContext<'_>,
    ) -> Result<(), SQLError> {
        self.projected_aggregate_plans.observe(
            &mut self.groups[index].state.accumulators,
            &self.aggregate_targets,
            row,
            &(*context).with_function_states(self.aggregate_targets.calls()),
        )
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
