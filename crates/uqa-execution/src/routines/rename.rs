//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rename routines inside the caller transaction and publish dependent definitions in catalog order.

use super::{
    catalog::RoutineMutationContext,
    compilation::{compile_persisted_sql_function, StoredRoutineCompilationContext},
};
use crate::schema::{
    namespaces::NamespaceCatalogRefresh, relation_alteration::RoleTargetSchemaAccess,
};
use std::{collections::BTreeMap, sync::Arc};
use uqa_sql::{
    ast::{FunctionBinding, RenameRoutineStmt},
    catalog::roles::role_inherits,
    routines::{
        declaration::resolve_routine_identity_types,
        lifecycle::{
            binding::resolve_sql_routine_alter_target,
            ensure_routine_owner_as,
            rename::{self as analysis, RoutineRenameTarget},
        },
        SQLUserFunction,
    },
    SQLError,
};
use uqa_storage::StorageBackendResult;

pub trait RoutineRenameDependents {
    fn rewrite_schema_routine_identity(
        &self,
        target: &FunctionBinding,
        new_name: &str,
    ) -> StorageBackendResult<()>;
    fn rewrite_view_routine_identity(
        &self,
        target: &FunctionBinding,
        new_name: &str,
    ) -> StorageBackendResult<()>;
    fn rewrite_event_routine_identity(
        &self,
        target: &FunctionBinding,
        new_name: &str,
    ) -> Result<(), SQLError>;
}
pub struct RoutineRenameContext<'a> {
    pub mutation: RoutineMutationContext<'a>,
    pub refresh: &'a dyn NamespaceCatalogRefresh,
    pub schemas: &'a dyn RoleTargetSchemaAccess,
    pub compilation: StoredRoutineCompilationContext<'a>,
    pub dependents: &'a dyn RoutineRenameDependents,
}

pub fn rename_sql_routine(
    context: &RoutineRenameContext<'_>,
    stmt: &RenameRoutineStmt,
) -> Result<(), SQLError> {
    context.mutation.writer.prepare_writer()?;
    context.refresh.refresh_catalog().map_err(|error| {
        SQLError::Internal(format!(
            "synchronize catalogs before routine rename: {error}"
        ))
    })?;
    let registry = context.mutation.registry.routine_snapshot();
    let target = resolve_routine_rename_target(context, stmt, &registry)?;
    let renamed_registry = analysis::move_routine_registry_entry(registry, &target)?;
    **context.mutation.registry.routines_write() = renamed_registry;

    let rewritten_registry =
        rewrite_routine_owned_dependency_identity(context, &target.binding, &target.new_name)?;
    **context.mutation.registry.routines_write() = rewritten_registry.clone();
    context
        .dependents
        .rewrite_schema_routine_identity(&target.binding, &target.new_name)
        .map_err(|error| {
            SQLError::Internal(format!("rewrite schema routine dependencies: {error}"))
        })?;
    context
        .dependents
        .rewrite_view_routine_identity(&target.binding, &target.new_name)
        .map_err(|error| {
            SQLError::Internal(format!("rewrite view routine dependencies: {error}"))
        })?;
    context
        .dependents
        .rewrite_event_routine_identity(&target.binding, &target.new_name)?;
    context
        .mutation
        .publication
        .persist_routine_definitions(&rewritten_registry)?;
    context.mutation.changes.catalog_registry_changed();
    Ok(())
}

fn resolve_routine_rename_target(
    context: &RoutineRenameContext<'_>,
    stmt: &RenameRoutineStmt,
    registry: &BTreeMap<String, Vec<Arc<SQLUserFunction>>>,
) -> Result<RoutineRenameTarget, SQLError> {
    let requested_types = resolve_routine_identity_types(
        context.compilation.analysis.types,
        stmt.arg_types.as_deref(),
        &stmt.arg_type_references,
        "ALTER routine RENAME",
    )?;
    let (old_name, position) = resolve_sql_routine_alter_target(
        context.mutation.names,
        registry,
        &stmt.name,
        requested_types.as_deref(),
        stmt.kind,
    )?;
    let function = registry
        .get(&old_name)
        .and_then(|overloads| overloads.get(position))
        .ok_or_else(|| {
            SQLError::Internal(format!(
                "resolved ALTER routine target `{old_name}` disappeared before rename"
            ))
        })?;
    let current_user = context.mutation.names.current_user_name();
    let roles = context.mutation.roles.role_definitions();
    let memberships = context.mutation.roles.role_memberships();
    ensure_routine_owner_as(
        &function.def,
        role_inherits(&roles, &memberships, &current_user, &function.def.owner),
    )?;
    drop(memberships);
    drop(roles);
    let old_identity = analysis::routine_rename_identity(&old_name)?;
    context
        .schemas
        .require_schema_create(&old_identity.schema, &current_user)?;
    analysis::finish_routine_rename_target(
        stmt,
        old_name,
        position,
        &function.def,
        &old_identity,
        registry,
    )
}

fn rewrite_routine_owned_dependency_identity(
    context: &RoutineRenameContext<'_>,
    target: &FunctionBinding,
    new_name: &str,
) -> Result<BTreeMap<String, Vec<Arc<SQLUserFunction>>>, SQLError> {
    let registry = context.mutation.registry.routine_snapshot();
    let mut rewritten = BTreeMap::new();
    for (name, overloads) in registry {
        let mut next_overloads = Vec::with_capacity(overloads.len());
        for function in overloads {
            let mut def = function.def.clone();
            let changed =
                analysis::rewrite_routine_owned_dependency_identity(&mut def, target, new_name)?;
            if changed {
                let compiled = compile_persisted_sql_function(&context.compilation, &def)?;
                next_overloads.push(Arc::new(SQLUserFunction { def, compiled }));
            } else {
                next_overloads.push(function);
            }
        }
        rewritten.insert(name, next_overloads);
    }
    Ok(rewritten)
}
