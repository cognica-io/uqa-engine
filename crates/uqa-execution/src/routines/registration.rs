//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Register and alter routines while retaining authorization and registry guards through persistence.

use super::{
    catalog::RoutineMutationContext,
    compilation::{apply_session_compile_options, with_routine_settings},
    configuration::{self, RoutineConfigurationSession},
    definition::{
        compile_catalog_bound_routine, BoundRoutine, RoutineBodyCompilation,
        RoutineDefinitionContext,
    },
    invocation::context::RoutineInvocationSession,
};
use crate::catalog::security::roles::{
    dependencies::{prepare_role_dependencies, RoleDependencyCandidate},
    locking::RoleLockContext,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
use uqa_sql::catalog::roles::identity::RoleSubject;
use uqa_sql::{
    ast::{AlterRoutineStmt, CreateFunction, FunctionBody, RoleAttribute},
    catalog::roles::role_inherits,
    routines::{
        attributes,
        body_validation::{validate_sql_function_body, SQLBodyValidationContext},
        declaration::{resolve_alter_routine_identity_types, resolve_routine_type_references},
        lifecycle::{
            alter_routine_kind_name, binding::resolve_sql_routine_alter_target,
            require_routine_ownership,
        },
        registration::{self as analysis, RoutineSupportAuthority},
        resolution::RoutineOverloadContext,
        routine_signature_types, CompiledFunctionBody, RoutineBody, SQLUserFunction,
    },
    SQLError,
};

pub struct RoutineRegistrationContext<'a> {
    pub catalog: RoutineMutationContext<'a>,
    pub namespace: crate::schema::namespaces::relations::RelationCreationContext<'a>,
    pub definition: RoutineDefinitionContext<'a>,
    pub support: &'a dyn RoutineSupportAuthority,
    pub configuration: &'a dyn RoutineConfigurationSession,
    pub overloads: RoutineOverloadContext<'a>,
    /// The session whose settings complete a `PL/pgSQL` body `CREATE FUNCTION` compiled.
    pub session: &'a dyn RoutineInvocationSession,
    /// The session that keeps the compilation validating a new body for its later calls.
    pub bodies: &'a dyn super::invocation::bodies::RoutineBodySession,
}

/// Whether `CREATE FUNCTION` examines the body, as `check_function_bodies` says.
fn checks_function_bodies(session: &dyn RoutineConfigurationSession) -> Result<bool, SQLError> {
    Ok(session.show_routine_variable("check_function_bodies")? == "on")
}

const fn body_compilation(checks_bodies: bool) -> RoutineBodyCompilation {
    if checks_bodies {
        RoutineBodyCompilation::Checked
    } else {
        RoutineBodyCompilation::Unchecked
    }
}

/// Validate a SQL body once the routine is visible, so that the body can call it, as `PostgreSQL` validates a body after it stores the routine: a SQL-standard body is analyzed whatever `check_function_bodies` says, and a body given as a string is analyzed under the routine's own settings only when it is on; the final statement is checked against the declared result only when it is on.
fn validate_registered_sql_body(
    context: &RoutineRegistrationContext<'_>,
    def: &CreateFunction,
    compiled: &CompiledFunctionBody,
    checks_bodies: bool,
) -> Result<(), SQLError> {
    let validation = SQLBodyValidationContext {
        compilation: context.definition.compilation.analysis,
        overloads: RoutineOverloadContext {
            catalog: context.overloads.catalog,
        },
    };
    match &def.body {
        FunctionBody::Statements(_) => {
            validate_sql_function_body(&validation, def, compiled, checks_bodies)
        }
        FunctionBody::Source(_) if checks_bodies => {
            with_routine_settings(&context.definition.compilation, def, || {
                validate_sql_function_body(&validation, def, compiled, true)
            })
        }
        FunctionBody::Source(_) => Ok(()),
    }
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

/// The OID reserved for a routine that the registry did not hold when its creation began.
fn created_routine_oid(name: &str, reserved: Option<u32>) -> Result<u32, SQLError> {
    reserved.ok_or_else(|| {
        SQLError::Internal(format!(
            "routine `{name}` replaced during creation disappeared"
        ))
    })
}

/// `ProcedureCreate` keeps a replaced routine's OID and gives a new routine the next one: `None` for a replacement. The OID is reserved before the registry is held for writing, since the reservation reads the catalog.
fn new_routine_oid(
    context: &RoutineRegistrationContext<'_>,
    name: &str,
    signature: &[String],
) -> Result<Option<u32>, SQLError> {
    let routines = context.catalog.registry.routine_snapshot();
    let replaces = routines.get(name).is_some_and(|overloads| {
        overloads
            .iter()
            .any(|function| routine_signature_types(&function.def) == signature)
    });
    if replaces {
        return Ok(None);
    }
    let oid = crate::catalog::identity::reserve_new_catalog_oid(
        context.namespace.locks,
        uqa_sql::schema::constraint_metadata::CatalogOidClass::Procedure.class_id(),
        "function",
        |oid| crate::catalog::projection::routine_oid_in_use(&routines, oid),
    )?;
    u32::try_from(oid)
        .map(Some)
        .map_err(|_| SQLError::Internal(format!("invalid routine OID {oid}")))
}

/// Check the statement as `CreateFunction` does, stage by stage: CREATE on the routine's schema; then, as `compute_function_attributes` interprets them, the attribute clauses in written order, the SET values, COST, ROWS, the SUPPORT function and PARALLEL; the language; LEAKPROOF; the transforms; the argument types with their defaults and the result type; the body; and whether ROWS applies. The locked registration checks the superuser-only attributes again.
fn validate_routine_creation(
    context: &RoutineRegistrationContext<'_>,
    def: &mut CreateFunction,
    current_user: &uqa_sql::catalog::roles::RoleReference,
) -> Result<(), SQLError> {
    context.namespace.ensure_create(&def.name)?;
    let clauses = std::mem::take(&mut def.attribute_clauses);
    attributes::check_attribute_clauses(&clauses, def.is_procedure)?;
    configuration::apply_routine_config_actions(context.configuration, def)?;
    attributes::validate_cost(def.cost)?;
    attributes::validate_rows(def.rows)?;
    if let Some(support) = def.support.as_deref() {
        analysis::validate_routine_support(context.support, support)?;
    }
    attributes::validate_parallel(&clauses)?;
    attributes::validate_routine_language(def)?;
    let current_user_is_superuser = current_user
        .role_definition(&context.catalog.roles.role_definitions())
        .is_some_and(|role| role.has(RoleAttribute::Superuser));
    analysis::validate_routine_security_attributes(def, current_user_is_superuser)?;
    let types = context.definition.compilation.analysis.types;
    attributes::validate_transforms(types, def, &clauses)?;
    resolve_routine_type_references(types, def)?;
    attributes::validate_body_form(def, &clauses)?;
    attributes::validate_rows_applicability(def.rows, def.returns_set())?;
    attributes::reject_window_function(def, &clauses)?;
    if def.language == "plpgsql" {
        context.configuration.load_language_library(&def.language);
    }
    Ok(())
}

pub fn register_sql_function(
    context: &RoutineRegistrationContext<'_>,
    mut def: CreateFunction,
) -> Result<(), SQLError> {
    let current_user = context.catalog.names.current_role();
    let requested_owner = match def.owner {
        Some(owner) => uqa_sql::catalog::roles::RoleReference::from_identity(
            owner,
            &context.catalog.roles.role_definitions(),
        )?,
        None => current_user.clone(),
    };
    let locks = RoleLockContext {
        roles: context.catalog.roles,
        session: context.namespace.locks,
    };
    let owner = locks.bind(&requested_owner)?;
    def.owner = Some(owner.identity());
    let requested_name = def.name.clone();
    def.name = context.namespace.persistent_name(&requested_name)?;
    validate_routine_creation(context, &mut def, &current_user)?;
    let checks_bodies = checks_function_bodies(context.configuration)?;
    let bound = compile_catalog_bound_routine(
        &context.definition,
        &mut def,
        body_compilation(checks_bodies),
    )?;
    let name = def.name.clone();
    let signature = routine_signature_types(&def);
    let new_oid = new_routine_oid(context, &name, &signature)?;
    let RoleDependencyCandidate {
        roles,
        memberships,
        value: (mut registry, next, published),
        ..
    } = prepare_role_dependencies(
        &locks,
        || context.catalog.writer.prepare_writer(),
        || {
            locks.revalidate(&owner)?;
            context.namespace.ensure_create(&name)?;
            let roles = context.catalog.roles.role_definitions();
            owner.revalidate(&roles)?;
            let current_user_is_superuser = current_user
                .role_definition(&roles)
                .is_some_and(|role| role.has(RoleAttribute::Superuser));
            let memberships = context.catalog.roles.role_memberships();
            analysis::validate_routine_security_attributes(&def, current_user_is_superuser)?;
            let registry = context.catalog.registry.routines_write();
            let mut next = registry.clone();
            let mut def = def.clone();
            let overloads = next.entry(name.clone()).or_default();
            let mut dependencies = BTreeSet::new();
            let published = if let Some(pos) = overloads
                .iter()
                .position(|function| routine_signature_types(&function.def) == signature)
            {
                let existing = &overloads[pos].def;
                analysis::prepare_routine_replacement(
                    existing,
                    &mut def,
                    &current_user,
                    &roles,
                    &memberships,
                )?;
                let published = super::catalog::revision::replacement(def, bound.body.clone())?;
                overloads[pos] = Arc::clone(&published);
                published
            } else {
                dependencies =
                    uqa_sql::routines::security::binding::routine_role_dependencies(&def, &roles)?;
                def.object_id = Some(allocate_routine_object_id(&registry, &name)?);
                def.catalog_oid = Some(created_routine_oid(&name, new_oid)?);
                let published = super::catalog::revision::replacement(def, bound.body.clone())?;
                overloads.push(Arc::clone(&published));
                published
            };
            overloads.sort_by(|left, right| {
                routine_signature_types(&left.def)
                    .cmp(&routine_signature_types(&right.def))
                    .then_with(|| left.def.is_procedure.cmp(&right.def.is_procedure))
            });
            Ok(RoleDependencyCandidate {
                value: (registry, next, published),
                memberships,
                roles,
                dependencies,
            })
        },
    )?;
    context
        .catalog
        .publication
        .persist_routine_definitions(&next)?;
    super::catalog::publication::record_changes(context.catalog.changes, &registry, &next);
    **registry = next;
    drop(registry);
    drop(memberships);
    drop(roles);
    finish_registration(context, &def, &published, &bound, checks_bodies)
}

/// Complete a published definition: the compilation that validated a `PL/pgSQL` body stays with the defining session, and a SQL body is validated now that the routine is visible, so that the body can call it. A failure aborts the statement, whose rollback withdraws the routine.
fn finish_registration(
    context: &RoutineRegistrationContext<'_>,
    def: &CreateFunction,
    published: &SQLUserFunction,
    bound: &BoundRoutine,
    checks_bodies: bool,
) -> Result<(), SQLError> {
    retain_validated_body(context, published, bound.validated.as_ref())?;
    context.catalog.changes.catalog_registry_changed();
    let compiled = match &bound.body {
        RoutineBody::Bound(body) => Some(body.as_ref()),
        RoutineBody::Source => bound.validated.as_ref(),
    };
    compiled.map_or(Ok(()), |compiled| {
        validate_registered_sql_body(context, def, compiled, checks_bodies)
    })
}

/// The PL/pgSQL validator leaves its compilation in the defining session's function cache, completed with the session's settings of the moment under the routine's own; the SQL validator does not, so a SQL body compiles when a session first runs it.
fn retain_validated_body(
    context: &RoutineRegistrationContext<'_>,
    published: &SQLUserFunction,
    validated: Option<&CompiledFunctionBody>,
) -> Result<(), SQLError> {
    let Some(CompiledFunctionBody::PLpgSQL(parsed)) = validated else {
        return Ok(());
    };
    let mut parsed = parsed.clone();
    with_routine_settings(&context.definition.compilation, &published.def, || {
        apply_session_compile_options(context.session, &mut parsed);
        Ok(())
    })?;
    context
        .bodies
        .retain_routine_body(published, CompiledFunctionBody::PLpgSQL(parsed))
}

/// Change mutable routine attributes without replacing its identity or compiled body.
pub fn alter_sql_routine(
    context: &RoutineRegistrationContext<'_>,
    stmt: &AlterRoutineStmt,
) -> Result<(), SQLError> {
    context.catalog.writer.prepare_writer()?;
    let requested_types =
        resolve_alter_routine_identity_types(context.definition.compilation.analysis.types, stmt)?;
    let current_user = context.catalog.names.current_role();
    let roles = context.catalog.roles.role_definitions();
    let current_user_is_superuser = current_user
        .role_definition(&roles)
        .is_some_and(|role| role.has(RoleAttribute::Superuser));
    let memberships = context.catalog.roles.role_memberships();
    // Lookup diagnostics read the type catalog, so the target resolves before the registry is held for writing.
    let snapshot = context.catalog.registry.routine_snapshot();
    let (name, position) = resolve_sql_routine_alter_target(
        context.catalog.names,
        &snapshot,
        &stmt.name,
        requested_types.as_deref(),
        stmt.kind,
    )?;
    let resolved_identity = snapshot[&name][position].def.object_id;
    let mut registry = context.catalog.registry.routines_write();
    let existing = registry
        .get(&name)
        .and_then(|overloads| overloads.get(position))
        .filter(|function| function.def.object_id == resolved_identity)
        .cloned()
        .ok_or_else(|| {
            SQLError::Internal(format!(
                "resolved ALTER routine target `{name}` changed before mutation"
            ))
        })?;
    // `AlterFunction` names the routine by the statement's object kind and the name as written.
    require_routine_ownership(
        alter_routine_kind_name(stmt.kind),
        &stmt.name,
        role_inherits(
            &roles,
            &memberships,
            &current_user,
            &uqa_sql::routines::security::bound_routine_owner(&existing.def)?,
        ),
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
    overloads[position] = super::catalog::revision::replacement(def, existing.body.clone())?;
    context
        .catalog
        .publication
        .persist_routine_definitions(&next)?;
    super::catalog::publication::record_changes(context.catalog.changes, &registry, &next);
    **registry = next;
    drop(registry);
    drop(memberships);
    drop(roles);
    context.catalog.changes.catalog_registry_changed();
    Ok(())
}
