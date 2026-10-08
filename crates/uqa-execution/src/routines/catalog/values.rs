//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain catalog tuple identity while replacing a routine's changed composite input datums and compiled SQL body.

use super::RoutineMutationContext;
use std::sync::Arc;
use uqa_sql::{
    ast::FunctionBody,
    expr::composites::constants::CompositeConstantChange,
    routines::{CompiledFunctionBody, RoutineBody},
    SQLError,
};

pub fn rewrite_composite_constants(
    context: &RoutineMutationContext<'_>,
    change: &CompositeConstantChange<'_>,
) -> Result<(), SQLError> {
    let before = context.registry.routine_snapshot();
    let mut next = before.clone();
    let mut changed = false;
    for routine in next.values_mut().flatten() {
        let mut definition = routine.def.clone();
        let mut revised = false;
        for parameter in &mut definition.params {
            if let Some(default) = &mut parameter.default {
                revised |= change.expression(default)?;
            }
        }
        if let FunctionBody::Statements(statements) = &mut definition.body {
            for statement in statements {
                revised |= change.statement_in_scope(statement, Some(&routine.def), None)?;
            }
        }
        if !revised {
            continue;
        }
        let mut body = routine.body.clone();
        if let RoutineBody::Bound(compiled) = &mut body {
            if let CompiledFunctionBody::SQL(plans) = Arc::make_mut(compiled) {
                for plan in plans {
                    if let Some(rename) = change.rename {
                        rename.bind_routine_plan(plan, &routine.def)?;
                    }
                    change.plan(plan)?;
                }
            }
        }
        *routine = if change.rename.is_some() {
            Arc::new(uqa_sql::routines::SQLUserFunction::new(definition, body))
        } else {
            super::revision::replacement(definition, body)?
        };
        changed = true;
    }
    if changed {
        context.publication.persist_routine_definitions(&next)?;
        super::publication::record_changes(context.changes, &before, &next);
        **context.registry.routines_write() = next;
        context.changes.catalog_registry_changed();
    }
    Ok(())
}
