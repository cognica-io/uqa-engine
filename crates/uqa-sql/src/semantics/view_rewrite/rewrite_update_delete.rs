//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordinary UPDATE/DELETE callers share the consuming view rewrite.

use super::{CteScope, DeletePlan, SQLError, UpdatePlan, ViewRewriteBindings, ViewRewriteContext};
use crate::plan::{CommandPlan, MutableCommandPlan};
use uqa_core::memory::ProductionControl;

pub fn rewrite_update_to_base(
    services: ViewRewriteContext<'_>,
    statement: &UpdatePlan,
    params: &[crate::SQLParam],
    inherited: Option<&CteScope>,
) -> Result<UpdatePlan, SQLError> {
    rewrite_update_rule_inputs(services, statement, params, inherited, &mut |_| Ok(true))?
        .ok_or_else(|| SQLError::Internal("ordinary view rewrite suppressed its input".into()))
}

pub fn rewrite_delete_to_base(
    services: ViewRewriteContext<'_>,
    statement: &DeletePlan,
    params: &[crate::SQLParam],
    inherited: Option<&CteScope>,
) -> Result<DeletePlan, SQLError> {
    rewrite_delete_rule_inputs(services, statement, params, inherited, &mut |_| Ok(true))?
        .ok_or_else(|| SQLError::Internal("ordinary view rewrite suppressed its input".into()))
}

pub fn rewrite_update_rule_inputs(
    services: ViewRewriteContext<'_>,
    statement: &UpdatePlan,
    params: &[crate::SQLParam],
    inherited: Option<&CteScope>,
    visit: &mut dyn FnMut(&mut UpdatePlan) -> Result<bool, SQLError>,
) -> Result<Option<UpdatePlan>, SQLError> {
    let control = ProductionControl::uncontrolled();
    let input = statement.copy_for_rewrite(&control)?;
    let output = super::commands::rewrite_rule_inputs_owned(
        services,
        input,
        params,
        inherited.map(ViewRewriteBindings::Ordinary),
        &mut |input| {
            let mut command = input
                .finish(&control)?
                .into_uncontrolled()
                .expect("ordinary view command");
            let CommandPlan::Update(plan) = &mut command else {
                unreachable!("UPDATE view command");
            };
            Ok(visit(plan)?.then(|| MutableCommandPlan::from_ordinary(command)))
        },
        &control,
    )?;
    output
        .map(|output| {
            let CommandPlan::Update(plan) = output
                .finish(&control)?
                .into_uncontrolled()
                .expect("ordinary view command")
            else {
                unreachable!("UPDATE view command");
            };
            Ok(*plan)
        })
        .transpose()
}

pub fn rewrite_delete_rule_inputs(
    services: ViewRewriteContext<'_>,
    statement: &DeletePlan,
    params: &[crate::SQLParam],
    inherited: Option<&CteScope>,
    visit: &mut dyn FnMut(&mut DeletePlan) -> Result<bool, SQLError>,
) -> Result<Option<DeletePlan>, SQLError> {
    let control = ProductionControl::uncontrolled();
    let input = statement.copy_for_rewrite(&control)?;
    let output = super::commands::rewrite_rule_inputs_owned(
        services,
        input,
        params,
        inherited.map(ViewRewriteBindings::Ordinary),
        &mut |input| {
            let mut command = input
                .finish(&control)?
                .into_uncontrolled()
                .expect("ordinary view command");
            let CommandPlan::Delete(plan) = &mut command else {
                unreachable!("DELETE view command");
            };
            Ok(visit(plan)?.then(|| MutableCommandPlan::from_ordinary(command)))
        },
        &control,
    )?;
    output
        .map(|output| {
            let CommandPlan::Delete(plan) = output
                .finish(&control)?
                .into_uncontrolled()
                .expect("ordinary view command")
            else {
                unreachable!("DELETE view command");
            };
            Ok(*plan)
        })
        .transpose()
}
