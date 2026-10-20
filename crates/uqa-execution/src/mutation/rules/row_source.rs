//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded physical inputs for one set-oriented rewrite action.

use std::collections::BTreeMap;

use uqa_core::CancellationToken;
use uqa_sql::plpgsql::VariableResolver;
use uqa_sql::semantics::rules::binding::{
    BoundSetOrientedAction, RetainedRuleInput, RuleColumnMetadata, RuleRowValues,
};
use uqa_sql::{SQLError, Statement};

use crate::physical::physical_exec_error;
use crate::query::sources::retained_source::RetainedRowScope;
use crate::spill::SpillBatchWriter;
use crate::{PhysicalRow, SpillBuffer};

#[cfg(test)]
mod tests;

pub(super) fn bind_retained_action(
    matching_rows: &[usize],
    rows: &[impl RuleRowValues],
    columns: &BTreeMap<String, RuleColumnMetadata>,
    work_mem: usize,
    cancellation: &CancellationToken,
    bind_action: &dyn Fn(&mut dyn VariableResolver) -> Result<Statement, SQLError>,
) -> Result<(BoundSetOrientedAction, RetainedRowScope), SQLError> {
    let input = RetainedRuleInput::new(columns);
    let mut spill = SpillBuffer::new(work_mem / 2);
    let mut writer = SpillBatchWriter::new(input.schema().clone()).map_err(physical_exec_error)?;
    let mut has_admitted_records = false;
    for index in matching_rows {
        cancellation.check()?;
        let row = rows.get(*index).ok_or_else(|| {
            SQLError::Internal("rewrite rule lost its qualified row image".into())
        })?;
        let values = input.row_values(row, *index)?;
        has_admitted_records |= values[..columns.len() * 2]
            .iter()
            .any(uqa_sql::expr::composites::literal::contains_records);
        writer
            .push(&mut spill, PhysicalRow::from_values(values))
            .map_err(physical_exec_error)?;
    }
    cancellation.check()?;
    writer.finish(&mut spill).map_err(physical_exec_error)?;
    let rows = spill
        .into_shared(input.schema().clone())
        .map_err(physical_exec_error)?;
    let scope = if has_admitted_records {
        RetainedRowScope::enter_admitted(input.relation(), rows, input.admitted_types())?
    } else {
        RetainedRowScope::enter(input.relation(), rows)?
    };
    let bound = input.bind_action(scope.descriptor().clone(), bind_action)?;
    Ok((bound, scope))
}
