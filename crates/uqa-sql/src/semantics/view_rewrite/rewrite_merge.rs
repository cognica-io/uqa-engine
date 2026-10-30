//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordinary MERGE callers share the consuming view rewrite.
use super::{CteScope, MergePlan, SQLError, ViewRewriteBindings, ViewRewriteContext};
use crate::plan::CommandPlan;
use uqa_core::memory::ProductionControl;

pub fn rewrite_merge_to_base(
    services: ViewRewriteContext<'_>,
    statement: &MergePlan,
    params: &[crate::SQLParam],
    inherited: Option<&CteScope>,
) -> Result<MergePlan, SQLError> {
    let control = ProductionControl::uncontrolled();
    let input = statement.copy_for_rewrite(&control)?;
    let input = super::commands::rewrite_merge_owned(
        services,
        input,
        params,
        inherited.map(ViewRewriteBindings::Ordinary),
        &control,
    )?;
    let CommandPlan::Merge(plan) = input
        .finish(&control)?
        .into_uncontrolled()
        .expect("ordinary view command")
    else {
        unreachable!("MERGE view command")
    };
    Ok(*plan)
}
