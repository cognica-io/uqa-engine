//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Restore retained event expressions and validate their bound function identities.
use super::definition::EventAnalysisContext;
use crate::{ast::CreateTrigger, catalog::resolution::RelationLookupMode};

/// Upgrade both retained rule syntax and its analyzed condition without dropping constants or query identities.
pub fn restore_rule_expressions(
    rule: &mut super::StoredRule,
    routines: &dyn crate::FunctionTypeResolver,
) -> Result<bool, crate::SQLError> {
    let mut changed = rule.definition.condition.is_some() && rule.bound_condition_plan().is_none();
    if let Some(condition) = &mut rule.definition.condition {
        changed |= condition.upgrade_legacy_serialized_dispatches();
        changed |= crate::binding::legacy_patterns::restore_expression(condition, routines)
            .map_err(crate::SQLError::Internal)?;
    }
    if let Some(condition) = &mut rule.condition_plan {
        changed |= crate::binding::legacy_patterns::restore_expression_plan(condition, routines);
        changed |= condition.upgrade_legacy_range_predicates()?;
    }
    for action in &mut rule.definition.actions {
        changed |= action.upgrade_legacy_serialized_dispatches();
        changed |= crate::binding::legacy_patterns::restore_statement(action, routines)?;
    }
    Ok(changed)
}

pub fn trigger_function_object_id(
    context: &EventAnalysisContext<'_>,
    definition: &CreateTrigger,
) -> Result<[u8; 16], String> {
    context
        .resolve_trigger_function(&definition.function, RelationLookupMode::Bound)
        .map_err(|error| format!("restore trigger function identity: {error}"))?
        .def
        .object_id
        .ok_or_else(|| {
            format!(
                "restore trigger catalog: function `{}` has no object identity",
                definition.function
            )
        })
}
