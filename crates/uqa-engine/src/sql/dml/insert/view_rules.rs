//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{BTreeSet, ColumnType, Engine, InsertPlan, SQLError};
pub(super) fn view_rule_insert_column_type(
    engine: &Engine,
    stmt: &InsertPlan,
    input_position: usize,
) -> Result<Option<ColumnType>, SQLError> {
    uqa_sql::semantics::rules::insert_inputs::view_rule_insert_column_type(
        engine.view_rewrite_context(),
        stmt,
        input_position,
    )
}
pub(super) fn required_view_rule_insert_input_positions(
    engine: &Engine,
    stmt: &InsertPlan,
) -> Result<Option<BTreeSet<usize>>, SQLError> {
    uqa_sql::semantics::rules::insert_inputs::required_view_rule_insert_input_positions(
        engine.rule_analysis_context(),
        stmt,
    )
}
