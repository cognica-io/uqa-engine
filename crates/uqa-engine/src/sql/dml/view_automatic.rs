//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Connect view-row execution and engine authorization to SQL-owned automatic view rewriting.

use super::{CteScope, Engine, SQLError};
use crate::StoredView;
pub(in crate::sql) use logical::{MergeViewTargetPath, ViewUpdatability};
use uqa_sql::binding::snapshot::BindingSnapshot;
use uqa_sql::semantics::view_rewrite::{self as logical};
use uqa_sql::{
    ast::{RuleEvent, TriggerEvent},
    plan::{DeletePlan, InsertPlan, MergePlan, UpdatePlan},
    RowSchema,
};

fn binding_snapshot(scope: &CteScope) -> Result<BindingSnapshot, SQLError> {
    uqa_execution::query::binding::binding_context(scope).map(BindingSnapshot::from)
}

pub(in crate::sql) fn has_instead_of_trigger(
    engine: &Engine,
    view: &str,
    event: TriggerEvent,
) -> Result<bool, SQLError> {
    logical::has_instead_of_trigger(engine.view_rewrite_context(), view, event)
}

pub(in crate::sql) fn view_updatability(
    engine: &Engine,
    name: &str,
) -> Result<ViewUpdatability, SQLError> {
    logical::view_updatability(engine.view_rewrite_context(), name)
}

pub(in crate::sql) fn validate_view_definition_check_option(
    engine: &Engine,
    name: &str,
    definition: &StoredView,
) -> Result<(), SQLError> {
    logical::validate_view_definition_check_option(
        engine.view_rewrite_context(),
        name,
        &definition.rewrite_definition(),
    )
}

pub(in crate::sql::dml) fn rewrite_insert_to_base(
    engine: &Engine,
    statement: &InsertPlan,
    params: &[uqa_sql::SQLParam],
    inherited_ctes: Option<&CteScope>,
) -> Result<InsertPlan, SQLError> {
    let inherited = inherited_ctes.map(binding_snapshot).transpose()?;
    logical::rewrite_insert_to_base(
        engine.view_rewrite_context(),
        statement,
        params,
        inherited.as_ref(),
    )
}

pub(in crate::sql::dml) fn rewrite_merge_to_base(
    engine: &Engine,
    statement: &MergePlan,
    params: &[uqa_sql::SQLParam],
    inherited_ctes: Option<&CteScope>,
) -> Result<MergePlan, SQLError> {
    let inherited = inherited_ctes.map(binding_snapshot).transpose()?;
    logical::rewrite_merge_to_base(
        engine.view_rewrite_context(),
        statement,
        params,
        inherited.as_ref(),
    )
}

pub(in crate::sql::dml) fn rewrite_update_to_base(
    engine: &Engine,
    statement: &UpdatePlan,
    params: &[uqa_sql::SQLParam],
    inherited_ctes: Option<&CteScope>,
) -> Result<UpdatePlan, SQLError> {
    let inherited = inherited_ctes.map(binding_snapshot).transpose()?;
    logical::rewrite_update_to_base(
        engine.view_rewrite_context(),
        statement,
        params,
        inherited.as_ref(),
    )
}

pub(in crate::sql::dml) fn rewrite_delete_to_base(
    engine: &Engine,
    statement: &DeletePlan,
    params: &[uqa_sql::SQLParam],
    inherited_ctes: Option<&CteScope>,
) -> Result<DeletePlan, SQLError> {
    let inherited = inherited_ctes.map(binding_snapshot).transpose()?;
    logical::rewrite_delete_to_base(
        engine.view_rewrite_context(),
        statement,
        params,
        inherited.as_ref(),
    )
}

pub(in crate::sql) fn validate_public_merge_contract(
    engine: &Engine,
    plan: &MergePlan,
    source: &RowSchema,
) -> Result<(), SQLError> {
    logical::validate_public_merge_contract(engine.view_rewrite_context(), plan, source)
}

pub(in crate::sql) fn validate_public_merge_targets(
    engine: &Engine,
    plan: &MergePlan,
) -> Result<(), SQLError> {
    logical::validate_public_merge_targets(engine.view_rewrite_context(), plan)
}

pub(in crate::sql::dml) fn merge_view_target_path(
    engine: &Engine,
    plan: &MergePlan,
) -> Result<MergeViewTargetPath, SQLError> {
    logical::merge_view_target_path(engine.view_rewrite_context(), plan)
}

use logical::rule_inputs::RuleInputRequirements;
pub(in crate::sql) fn rule_input_requirements(
    engine: &Engine,
    table: &str,
    event: RuleEvent,
) -> Result<Option<RuleInputRequirements>, SQLError> {
    logical::rule_input_requirements(engine.view_rewrite_context(), table, event)
}
