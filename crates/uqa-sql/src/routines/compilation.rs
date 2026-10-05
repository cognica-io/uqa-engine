//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine language validation and body lowering over declared types and fresh binding inputs.

use super::{
    declaration::{
        resolve_plpgsql_datum_types, routine_definition_error, routine_parameter_regrole_constants,
        validate_routine_declaration, RoutineTypeCatalog,
    },
    merge_columns::StoredMergeColumnCatalog,
    CompiledFunctionBody, RoutineResolution,
};
use crate::{
    ast::{ColumnType, CreateFunction, FunctionBody, FunctionReturns, Statement},
    binding::{
        snapshot::BindingSnapshot,
        stored_relations::{
            self, StoredQueryBindingContext, StoredQueryNamespace, StoredQuerySequences,
            StoredRelationCatalog,
        },
    },
    catalog::regrole_dependencies::{StoredRegroleConstants, StoredRegroleResolver},
    plan::UnifiedPlan,
    plpgsql::PlpgsqlCatalog,
    type_resolution::canonical_routine_type_name,
    SQLError, ScalarExpr,
};

pub trait RoutineParserCatalog {
    fn plpgsql_catalog(&self) -> Result<PlpgsqlCatalog, SQLError>;
}
pub trait RoutineCompilationCatalog {
    fn has_registered_aggregate_function(&self, name: &str) -> bool;
    fn binding_snapshot(&self) -> Result<BindingSnapshot, SQLError>;
    fn stored_query_namespace(&self) -> StoredQueryNamespace;
}
#[derive(Clone, Copy)]
pub struct RoutineCompilationContext<'a> {
    pub types: &'a dyn RoutineTypeCatalog,
    pub parsers: &'a dyn RoutineParserCatalog,
    pub catalog: &'a dyn RoutineCompilationCatalog,
    pub routines: &'a dyn RoutineResolution,
    pub relations: &'a dyn StoredRelationCatalog,
    pub sequences: &'a dyn StoredQuerySequences,
    pub merge: &'a dyn StoredMergeColumnCatalog,
    pub regroles: &'a dyn StoredRegroleResolver,
}

pub fn compile_function_body(
    context: &RoutineCompilationContext<'_>,
    def: &CreateFunction,
) -> Result<CompiledFunctionBody, SQLError> {
    compile_function_body_inner(context, def, false)
}

pub fn compile_persisted_function_body(
    context: &RoutineCompilationContext<'_>,
    def: &CreateFunction,
) -> Result<CompiledFunctionBody, SQLError> {
    compile_function_body_inner(context, def, true)
}

/// The body `CREATE FUNCTION` compiles under `check_function_bodies = off`: the declaration is checked as always, a SQL-standard body, which the statement itself analyzes, is compiled, and a body given as a string is left unexamined, `None`, for each session to compile when it first calls the routine.
pub fn defer_function_body(
    context: &RoutineCompilationContext<'_>,
    def: &CreateFunction,
) -> Result<Option<CompiledFunctionBody>, SQLError> {
    if matches!(def.body, FunctionBody::Statements(_)) {
        return compile_function_body(context, def).map(Some);
    }
    validate_routine_signature(context, def)?.reject_with(context.regroles)?;
    Ok(None)
}

/// The checks `CREATE FUNCTION` makes whatever `check_function_bodies` says: the language and the body form it accepts, the declared types, and the role constants of parameter defaults, which are returned for the body's own checks.
fn validate_routine_signature(
    context: &RoutineCompilationContext<'_>,
    def: &CreateFunction,
) -> Result<StoredRegroleConstants, SQLError> {
    if !matches!(def.language.as_str(), "plpgsql" | "sql") {
        return Err(SQLError::Routine {
            sqlstate: "42704".into(),
            message: format!("language \"{}\" does not exist", def.language),
        });
    }
    if def.language == "plpgsql" && matches!(def.body, FunctionBody::Statements(_)) {
        return Err(routine_definition_error(
            "inline SQL function body only valid for language SQL",
        ));
    }
    let stored_regrole_constants = routine_parameter_regrole_constants(context.types, def);
    stored_regrole_constants.validate_inputs_with(context.regroles)?;
    validate_routine_declaration(def)?;
    Ok(stored_regrole_constants)
}

/// PL/pgSQL's compiler rejects declared arguments of a trigger function, which reads its arguments from `TG_ARGV`.
fn reject_trigger_function_arguments(def: &CreateFunction) -> Result<(), SQLError> {
    let returns_trigger = matches!(
        &def.returns,
        FunctionReturns::Scalar { type_name } if canonical_routine_type_name(type_name) == "trigger"
    );
    if returns_trigger && def.identity_arity() != 0 {
        return Err(SQLError::Diagnostic {
            sqlstate: "42P13".into(),
            message: "trigger functions cannot have declared arguments".into(),
            detail: None,
            hint: Some(
                "The arguments of the trigger can be accessed through TG_NARGS and TG_ARGV instead."
                    .into(),
            ),
        });
    }
    Ok(())
}

fn compile_function_body_inner(
    context: &RoutineCompilationContext<'_>,
    def: &CreateFunction,
    persisted_definition: bool,
) -> Result<CompiledFunctionBody, SQLError> {
    let mut stored_regrole_constants = validate_routine_signature(context, def)?;
    match def.language.as_str() {
        "plpgsql" => {
            stored_regrole_constants.reject_with(context.regroles)?;
            reject_trigger_function_arguments(def)?;
            let catalog = context.parsers.plpgsql_catalog()?;
            let mut function = crate::plpgsql::parse_function_with_catalog(def, &catalog)?;
            resolve_plpgsql_datum_types(context.types, &mut function)?;
            Ok(CompiledFunctionBody::PLpgSQL(function))
        }
        "sql" => {
            let (statements, bind_catalog_dependencies) = match &def.body {
                FunctionBody::Source(source) => {
                    stored_regrole_constants.reject_with(context.regroles)?;
                    (crate::compile(source)?, false)
                }
                FunctionBody::Statements(statements) => (statements.clone(), true),
            };
            let mut plans = compile_sql_routine_plans(
                context,
                def,
                statements,
                bind_catalog_dependencies,
                persisted_definition && matches!(def.body, FunctionBody::Statements(_)),
            )?;
            if bind_catalog_dependencies {
                for plan in &mut plans {
                    stored_regrole_constants.collect_plan(plan);
                }
                stored_regrole_constants.reject_with(context.regroles)?;
            }
            Ok(CompiledFunctionBody::SQL(plans))
        }
        _ => unreachable!("routine language was validated above"),
    }
}

fn compile_sql_routine_plans(
    context: &RoutineCompilationContext<'_>,
    def: &CreateFunction,
    statements: Vec<Statement>,
    bind_catalog_dependencies: bool,
    persisted_definition: bool,
) -> Result<Vec<UnifiedPlan>, SQLError> {
    let positional_parameters =
        super::body_validation::routine_parameter_values(context.types, def);
    let parameters = super::body_parameters::sql_body_parameter_scope(def, &positional_parameters)?;
    statements
        .into_iter()
        .map(|statement| {
            let mut plan = lower_sql_routine_statement(
                context,
                statement,
                SQLRoutineLowering {
                    bind_catalog_dependencies,
                    persisted_definition,
                    preserve_target_expressions: false,
                },
            )?;
            // A SQL-standard body is analyzed when the routine is defined, so its names resolve against the catalog of that moment, as `PostgreSQL` stores the analyzed statements. A body given as a string keeps its names until each statement is analyzed before it runs, and keeps the types the session resolved when it compiled the body.
            if bind_catalog_dependencies {
                let binding = context.catalog.binding_snapshot()?;
                crate::binding::bind_routine_parameter_references(
                    context.routines,
                    &mut plan,
                    &positional_parameters,
                    &binding.context(),
                    &parameters,
                )?;
                if let UnifiedPlan::Query(query) = &mut plan {
                    crate::binding::bind_query_plan_routines_for_storage(
                        context.routines,
                        query,
                        &positional_parameters,
                        &binding.context(),
                        None,
                    )?;
                }
            } else {
                bind_session_plan_types(context, &mut plan)?;
            }
            // Stored definitions retain their analyzed logical expressions;
            // immutable evaluation belongs to invocation planning.
            Ok(plan)
        })
        .collect()
}

/// How a SQL routine statement is lowered from its stored syntax.
#[derive(Clone, Copy)]
pub struct SQLRoutineLowering {
    /// Bind relations, sequences and `MERGE` target columns of catalog-owned syntax.
    pub bind_catalog_dependencies: bool,
    /// The syntax comes from a persisted definition, whose legacy call markers are upgraded.
    pub persisted_definition: bool,
    /// Keep `MERGE` target columns as written.
    pub preserve_target_expressions: bool,
}

/// Lower one statement of a SQL routine body with its relations bound, before routine calls are bound.
pub fn lower_sql_routine_statement(
    context: &RoutineCompilationContext<'_>,
    mut statement: Statement,
    lowering: SQLRoutineLowering,
) -> Result<UnifiedPlan, SQLError> {
    if lowering.bind_catalog_dependencies {
        validate_sql_standard_statement(&statement)?;
    }
    if lowering.bind_catalog_dependencies && !lowering.preserve_target_expressions {
        super::merge_columns::normalize_stored_merge_target_columns(context.merge, &mut statement)?;
    }
    let mut plan = UnifiedPlan::lower_with(statement, &|name: &str| {
        context.catalog.has_registered_aggregate_function(name)
    });
    if lowering.persisted_definition {
        plan.rewrite_scalar_expressions(&mut |expression| {
            let ScalarExpr::Func { name, binding, .. } = expression else {
                return;
            };
            crate::ast::FunctionBinding::upgrade_legacy_serialized_dispatch(name, binding);
        });
    }
    if lowering.bind_catalog_dependencies {
        match &mut plan {
            UnifiedPlan::Query(query) => {
                let namespace = context.catalog.stored_query_namespace();
                stored_relations::bind_stored_query_relations(
                    &StoredQueryBindingContext {
                        relations: context.relations,
                        sequences: context.sequences,
                        temporary_schema: &namespace.temporary_schema,
                        transition_relations: &namespace.transition_relations,
                    },
                    query,
                    "SQL routine body",
                    false,
                    lowering.persisted_definition,
                )?;
            }
            UnifiedPlan::Command(_) => {
                crate::binding::stored_routines::mark_catalog_statement_relations_bound(&mut plan)?;
            }
        }
    }
    Ok(plan)
}

/// A foreign-table utility is analyzed when it executes and cannot be retained as an analyzed SQL-standard body. Quoted source bodies keep their separate execution-time path.
pub(super) fn validate_sql_standard_statement(statement: &Statement) -> Result<(), SQLError> {
    if matches!(
        statement,
        Statement::CreateForeignTable(_) | Statement::CreateForeignTableDefinition(_)
    ) {
        return Err(SQLError::Unsupported(
            "CREATE FOREIGN TABLE is not yet supported in unquoted SQL function body".into(),
        ));
    }
    Ok(())
}

/// A session's compilation of a source body keeps the types it resolved, as its analyzed plan holds type OIDs that renaming an enum does not invalidate. A domain coercion records the domain as a plan dependency, so a changed domain is resolved again, and relations and routines are resolved when the plan runs.
fn bind_session_plan_types(
    context: &RoutineCompilationContext<'_>,
    plan: &mut UnifiedPlan,
) -> Result<(), SQLError> {
    crate::binding::stored_types::bind_unified_plan_type_identities(plan, &mut |name| {
        let resolved = context.types.resolve_catalog_column_type(name);
        let mut element = resolved.as_ref();
        while let Some(ColumnType::Array(inner)) = element {
            element = Some(inner.as_ref());
        }
        let domain = matches!(element, Some(ColumnType::Domain { .. }));
        Ok(resolved.filter(|_| !domain))
    })
}

#[cfg(test)]
mod tests;
