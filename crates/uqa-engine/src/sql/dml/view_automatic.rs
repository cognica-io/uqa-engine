//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Connect view-row execution and engine authorization to SQL-owned automatic view rewriting.

use super::{Engine, SQLError};
use crate::StoredView;
pub(in crate::sql) use logical::ViewUpdatability;
use uqa_sql::ast::{RuleEvent, TriggerEvent};
use uqa_sql::semantics::view_rewrite::{self as logical};

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

use logical::rule_inputs::RuleInputRequirements;
pub(in crate::sql) fn rule_input_requirements(
    engine: &Engine,
    table: &str,
    event: RuleEvent,
) -> Result<Option<RuleInputRequirements>, SQLError> {
    logical::rule_input_requirements(engine.view_rewrite_context(), table, event)
}
