//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordinary INSERT callers share the consuming view rewrite.
use super::{CteScope, InsertPlan, SQLError, ViewRewriteBindings, ViewRewriteContext};
use crate::plan::CommandPlan;
use uqa_core::memory::ProductionControl;

pub fn rewrite_insert_to_base(
    services: ViewRewriteContext<'_>,
    statement: &InsertPlan,
    params: &[crate::SQLParam],
    inherited: Option<&CteScope>,
) -> Result<InsertPlan, SQLError> {
    let control = ProductionControl::uncontrolled();
    let input = statement.copy_for_rewrite(&control)?;
    let input = super::commands::rewrite_insert_owned(
        services,
        input,
        params,
        inherited.map(ViewRewriteBindings::Ordinary),
        &control,
    )?;
    let CommandPlan::Insert(plan) = input
        .finish(&control)?
        .into_uncontrolled()
        .expect("ordinary view command")
    else {
        unreachable!("INSERT view command")
    };
    Ok(*plan)
}
