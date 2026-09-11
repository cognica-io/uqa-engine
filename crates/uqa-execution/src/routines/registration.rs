//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Register and alter routines while retaining authorization and registry guards through persistence.

use super::{
    catalog::RoutineMutationContext,
    configuration::{self, RoutineConfigurationSession},
    definition::{compile_catalog_bound_routine, RoutineDefinitionContext},
};
use std::{collections::BTreeMap, sync::Arc};
use uqa_sql::{
    ast::{AlterRoutineStmt, CreateFunction, RoleAttribute},
    catalog::roles::{require_role_exists, role_inherits},
    routines::{
        declaration::{resolve_alter_routine_identity_types, resolve_routine_type_references},
        dependencies::RoutineCompilationMode,
        lifecycle::{binding::resolve_sql_routine_alter_target, ensure_routine_owner_as},
        registration::{self as analysis, RoutineSupportAuthority},
        routine_signature_types, SQLUserFunction,
    },
    SQLError,
};

pub trait RoutineCreationNamespace {
    fn routine_name_for_create(&self, name: &str) -> Result<String, SQLError>;
}
pub struct RoutineRegistrationContext<'a> {
    pub catalog: RoutineMutationContext<'a>,
    pub namespace: &'a dyn RoutineCreationNamespace,
    pub definition: RoutineDefinitionContext<'a>,
    pub support: &'a dyn RoutineSupportAuthority,
    pub configuration: &'a dyn RoutineConfigurationSession,
}

fn allocate_routine_object_id(
    registry: &BTreeMap<String, Vec<Arc<SQLUserFunction>>>,
    name: &str,
) -> Result<[u8; 16], SQLError> {
    loop {
        let candidate =
            crate::catalog::identity::new_nonzero_catalog_identity("routine", "object identity")
                .map_err(|error| {
                    SQLError::Internal(format!("allocate routine `{name}` identity: {error}"))
                })?;
        if registry
            .values()
            .flat_map(|overloads| overloads.iter())
            .all(|function| function.def.object_id != Some(candidate))
        {
            return Ok(candidate);
        }
    }
}

pub fn register_sql_function(
    context: &RoutineRegistrationContext<'_>,
    mut def: CreateFunction,
) -> Result<(), SQLError> {
    context.catalog.writer.prepare_writer()?;
    let requested_name = def.name.clone();
    def.name = context.namespace.routine_name_for_create(&requested_name)?;
    resolve_routine_type_references(context.definition.compilation.analysis.types, &mut def)?;
    if def.owner.is_empty() {
        def.owner = context.catalog.names.current_user_name();
    }
    if let Some(support) = def.support.as_deref() {
        analysis::validate_routine_support(context.support, support)?;
    }
    configuration::apply_routine_config_actions(context.configuration, &mut def)?;
    let (compiled, _) = compile_catalog_bound_routine(
        &context.definition,
        &mut def,
        RoutineCompilationMode::Definition,
    )?;
    let name = def.name.clone();
    let signature = routine_signature_types(&def);
    let current_user = context.catalog.names.current_user_name();
    let roles = context.catalog.roles.role_definitions();
    require_role_exists(&roles, &def.owner)?;
    let current_user_is_superuser = roles
        .get(&current_user)
        .is_some_and(|role| role.has(RoleAttribute::Superuser));
    let memberships = context.catalog.roles.role_memberships();
    analysis::validate_routine_security_attributes(&def, current_user_is_superuser)?;
    let mut registry = context.catalog.registry.routines_write();
    let mut next = registry.clone();
    {
        let overloads = next.entry(name.clone()).or_default();
        if let Some(pos) = overloads
            .iter()
            .position(|function| routine_signature_types(&function.def) == signature)
        {
            let existing = &overloads[pos].def;
            analysis::prepare_routine_replacement(
                existing,
                &mut def,
                &requested_name,
                &current_user,
                &roles,
                &memberships,
            )?;
            overloads[pos] = Arc::new(SQLUserFunction { def, compiled });
        } else {
            def.object_id = Some(allocate_routine_object_id(&registry, &name)?);
            overloads.push(Arc::new(SQLUserFunction { def, compiled }));
        }
        overloads.sort_by(|left, right| {
            routine_signature_types(&left.def)
                .cmp(&routine_signature_types(&right.def))
                .then_with(|| left.def.is_procedure.cmp(&right.def.is_procedure))
        });
    }
    context
        .catalog
        .publication
        .persist_routine_definitions(&next)?;
    **registry = next;
    drop(registry);
    drop(memberships);
    drop(roles);
    context.catalog.changes.catalog_registry_changed();
    Ok(())
}

/// Change mutable routine attributes without replacing its identity or compiled body.
pub fn alter_sql_routine(
    context: &RoutineRegistrationContext<'_>,
    stmt: &AlterRoutineStmt,
) -> Result<(), SQLError> {
    context.catalog.writer.prepare_writer()?;
    let requested_types =
        resolve_alter_routine_identity_types(context.definition.compilation.analysis.types, stmt)?;
    let current_user = context.catalog.names.current_user_name();
    let roles = context.catalog.roles.role_definitions();
    let current_user_is_superuser = roles
        .get(&current_user)
        .is_some_and(|role| role.has(RoleAttribute::Superuser));
    let memberships = context.catalog.roles.role_memberships();
    let mut registry = context.catalog.registry.routines_write();
    let (name, position) = resolve_sql_routine_alter_target(
        context.catalog.names,
        &registry,
        &stmt.name,
        requested_types.as_deref(),
        stmt.kind,
    )?;
    let existing = registry
        .get(&name)
        .and_then(|overloads| overloads.get(position))
        .cloned()
        .ok_or_else(|| {
            SQLError::Internal(format!(
                "resolved ALTER routine target `{name}` disappeared before mutation"
            ))
        })?;
    ensure_routine_owner_as(
        &existing.def,
        role_inherits(&roles, &memberships, &current_user, &existing.def.owner),
    )?;
    let mut def = analysis::alter_routine_attributes(
        &existing.def,
        stmt,
        current_user_is_superuser,
        context.support,
    )?;
    configuration::apply_routine_config_actions(context.configuration, &mut def)?;
    let mut next = registry.clone();
    let overloads = next.get_mut(&name).ok_or_else(|| {
        SQLError::Internal(format!(
            "resolved ALTER routine registry entry `{name}` disappeared before mutation"
        ))
    })?;
    overloads[position] = Arc::new(SQLUserFunction {
        def,
        compiled: existing.compiled.clone(),
    });
    context
        .catalog
        .publication
        .persist_routine_definitions(&next)?;
    **registry = next;
    drop(registry);
    drop(memberships);
    drop(roles);
    context.catalog.changes.catalog_registry_changed();
    Ok(())
}
