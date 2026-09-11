//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine DROP preflight and ordered dependency-aware publication.

mod aliases;
mod cascade;
mod columns;
pub mod context;
mod dependencies;
mod labels;
mod publication;
mod relations;

pub use aliases::prepare_routine_column_alias_drop;
pub use cascade::{drop_domain_types_and_routines, drop_schema_types_and_routines};
use cascade::{expand_routine_domain_column_drop, expand_routine_domain_drop};
pub use columns::{drop_column_routine_dependents, expand_column_drop_dependencies};
pub use context::RoutineRemovalContext;
use dependencies::routine_object_dependents;
pub use labels::relation_dependents_drop_error;
use labels::routine_drop_display_label;
pub use publication::commit_sql_function_drop;
use relations::relation_drop_closure;
pub use relations::{drop_relation_routine_dependents, sequence_drop_column_names};

use crate::schema::domains::dependencies as domain_dependencies;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
use uqa_sql::{
    ast::DropFunctionStmt,
    routines::{
        lifecycle::{
            binding as analysis_binding,
            dependencies::{self as analysis_dependencies, append_schema_function_dependents},
            diagnostics::{append_routine_cascade_notice, ensure_no_function_dependencies},
            relations as analysis_relations, RoutineDropResolution, RoutineDropTarget,
            RoutineObjectDependents, RoutineSchemaDependents, SQLFunctionDropPlan,
        },
        routine_signature_types, SQLUserFunction,
    },
    SQLError,
};

pub fn drop_sql_functions(
    context: &RoutineRemovalContext<'_>,
    statement: &DropFunctionStmt,
) -> Result<(), SQLError> {
    let plan = preflight_sql_function_drop(context, statement)?;
    commit_sql_function_drop(context, plan)
}

/// Resolve every target and dependency before acquiring the registry write lock. Dependency scans take table and view locks, so keeping them out of the registry critical section preserves the catalog lock order.
pub fn preflight_sql_function_drop(
    context: &RoutineRemovalContext<'_>,
    stmt: &DropFunctionStmt,
) -> Result<SQLFunctionDropPlan, SQLError> {
    let kind = if stmt.is_procedure {
        "procedure"
    } else {
        "function"
    };
    let registry = context.registry.routine_snapshot();
    let mut resolution =
        analysis_binding::resolve_sql_function_drop_targets(context.names, stmt, &registry, kind)?;
    ensure_routine_drop_owners(context, &registry, &resolution.targets)?;
    let cascaded_routines =
        expand_stored_routine_drop_dependents(context, &registry, stmt.cascade, &mut resolution)?;
    let mut domains = BTreeSet::new();
    if stmt.cascade {
        expand_routine_domain_drop(context, &registry, &mut resolution, &mut domains)?;
    } else {
        let bindings = resolution
            .targets
            .iter()
            .map(RoutineDropTarget::binding)
            .collect::<Vec<_>>();
        domain_dependencies::expand_domain_drop_targets(&context.domains, &mut domains, &bindings)?;
        if !domains.is_empty()
            || !domain_dependencies::domain_checks_depending_on_routines(
                &context.domains,
                &bindings,
            )?
            .is_empty()
        {
            let label = routine_drop_display_label(context, &resolution.targets[0])?;
            return Err(SQLError::Routine {
                sqlstate: "2BP01".into(),
                message: format!("cannot drop function {label} because other objects depend on it"),
            });
        }
    }
    let dependents = routine_object_dependents(context, &resolution.targets, stmt.cascade)?;
    if stmt.cascade {
        append_routine_cascade_notice(&mut resolution.notices, &cascaded_routines, &dependents);
    }
    Ok(SQLFunctionDropPlan {
        domains,
        targets: resolution.targets,
        dependents,
        notices: resolution.notices,
    })
}

pub fn ensure_routine_drop_owners(
    context: &RoutineRemovalContext<'_>,
    registry: &BTreeMap<String, Vec<Arc<SQLUserFunction>>>,
    targets: &[RoutineDropTarget],
) -> Result<(), SQLError> {
    let current_user = context.catalog.current_user_name();
    let roles = context.roles.role_definitions();
    let memberships = context.roles.role_memberships();
    analysis_binding::ensure_routine_drop_owners(
        registry,
        targets,
        &current_user,
        &roles,
        &memberships,
    )
}

pub fn expand_stored_routine_drop_dependents(
    context: &RoutineRemovalContext<'_>,
    registry: &BTreeMap<String, Vec<Arc<SQLUserFunction>>>,
    cascade: bool,
    resolution: &mut RoutineDropResolution,
) -> Result<Vec<RoutineDropTarget>, SQLError> {
    analysis_dependencies::expand_stored_routine_drop_dependents(
        registry,
        cascade,
        resolution,
        &|target| routine_drop_display_label(context, target),
    )
}
