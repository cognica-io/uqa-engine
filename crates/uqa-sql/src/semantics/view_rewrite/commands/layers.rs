//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Each automatic view layer retains its own schema and expression constructors through translation.

use super::super::{
    production::{view_query_shape_owned, OwnedViewQueryShape},
    view_not_updatable, AutomaticViewLayer, NotUpdatableReason, SQLError, ViewCommand,
    ViewRewriteContext,
};
use crate::ast::RuleEvent;
use uqa_core::memory::{Produced, ProductionControl};

pub(super) fn direct_rule_path(
    services: ViewRewriteContext<'_>,
    view: &str,
    event: RuleEvent,
    command: ViewCommand,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let rules = services
        .catalog
        .capture_rules_with_control(view, event, control)?;
    let conditional =
        rules.any(|rule| rule.definition.instead && rule.definition.condition.is_some())?;
    let unconditional =
        rules.any(|rule| rule.definition.instead && rule.definition.condition.is_none())?;
    if conditional && !unconditional {
        return Err(view_not_updatable(
            view,
            command,
            NotUpdatableReason::ConditionalInsteadRule,
        ));
    }
    Ok(())
}

pub(super) fn rewritable(
    services: ViewRewriteContext<'_>,
    view: &str,
    command: ViewCommand,
    control: &ProductionControl<'_>,
) -> Result<Produced<AutomaticViewLayer>, SQLError> {
    match view_query_shape_owned(services, view, control)? {
        Some(OwnedViewQueryShape::Updatable(layer)) => layer.finish(control),
        Some(OwnedViewQueryShape::NotUpdatable(reason)) => {
            Err(view_not_updatable(view, command, reason))
        }
        None => Err(SQLError::UnknownTable(view.to_owned())),
    }
}

pub(super) fn next(
    services: ViewRewriteContext<'_>,
    view: &str,
    initial: &mut Option<Produced<AutomaticViewLayer>>,
    suppressed: bool,
    event: RuleEvent,
    command: ViewCommand,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<AutomaticViewLayer>>, SQLError> {
    if let Some(layer) = initial.take() {
        return Ok(Some(layer));
    }
    if !suppressed {
        direct_rule_path(services, view, event, command, control)?;
    }
    match view_query_shape_owned(services, view, control)? {
        Some(OwnedViewQueryShape::Updatable(layer)) => Ok(Some(layer.finish(control)?)),
        shape => {
            if suppresses(services, view, event, control)? {
                return Ok(None);
            }
            Err(match shape {
                Some(OwnedViewQueryShape::NotUpdatable(reason)) => {
                    view_not_updatable(view, command, reason)
                }
                _ => SQLError::UnknownTable(view.to_owned()),
            })
        }
    }
}

pub(super) fn suppresses(
    services: ViewRewriteContext<'_>,
    view: &str,
    event: RuleEvent,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    services
        .catalog
        .capture_rules_with_control(view, event, control)?
        .any(|rule| rule.definition.instead && rule.definition.condition.is_none())
}

pub(super) fn returns_rows(
    services: ViewRewriteContext<'_>,
    view: &str,
    event: RuleEvent,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    services
        .catalog
        .capture_rules_with_control(view, event, control)?
        .any(|rule| {
            rule.definition
                .actions
                .iter()
                .any(crate::semantics::rules::statement_has_returning)
        })
}
