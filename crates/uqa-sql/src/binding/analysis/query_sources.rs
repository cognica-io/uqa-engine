//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Source-owned retrieval scores and deferred wildcard descriptors.

use crate::{plan::QueryBlockPlan, RowSchema};
mod production;
pub(in crate::binding) use production::{
    with_projected_open_columns_with_control, with_query_source_columns_with_control,
};

pub(in crate::binding) fn with_query_source_columns(
    schema: &RowSchema,
    block: &QueryBlockPlan,
) -> RowSchema {
    with_query_source_columns_with_control(
        schema,
        block,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
    .expect("ordinary source namespace production")
}
