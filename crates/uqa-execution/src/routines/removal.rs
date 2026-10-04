//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `DROP FUNCTION` and `DROP PROCEDURE`: the named routines and what depends on them, removed as the catalog's dependencies order them.

pub mod context;
mod publication;

pub use context::RoutineRemovalContext;
pub use publication::commit_routine_registry_drop;

use uqa_sql::{ast::DropFunctionStmt, routines::lifecycle::binding as analysis_binding, SQLError};

pub fn drop_sql_functions(
    context: &RoutineRemovalContext<'_>,
    statement: &DropFunctionStmt,
) -> Result<(), SQLError> {
    let kind = if statement.is_procedure {
        "procedure"
    } else {
        "function"
    };
    let registry = context.registry.routine_snapshot();
    let current_user = context.catalog.current_role();
    let resolution = analysis_binding::resolve_sql_function_drop_targets(
        context.names,
        context.bodies.compilation.analysis.types,
        statement,
        &registry,
        kind,
        |function, item| {
            let roles = context.roles.role_definitions();
            let memberships = context.roles.role_memberships();
            analysis_binding::ensure_routine_drop_owner(
                context.names,
                function,
                &item.name,
                kind,
                &current_user,
                &roles,
                &memberships,
            )
        },
    )?;
    drop(registry);
    for notice in resolution.notices {
        context.notices.routine_drop_notice(notice);
    }
    let targets = resolution.targets;
    crate::schema::deletion::perform_deletion(
        &context.deletion.catalog_removal_context(),
        |dependencies| {
            targets
                .iter()
                .map(|target| {
                    crate::schema::deletion::required_address(
                        target
                            .object_id
                            .and_then(|object_id| dependencies.routine_address(&object_id)),
                        || format!("{} {}", target.kind(), target.name),
                    )
                })
                .collect()
        },
        statement.cascade,
    )
}
