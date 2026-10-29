//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine language validation and body lowering over declared types and fresh binding inputs.

mod ownership;
pub use ownership::bind_analyzed_sql_body_types_owned;

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
    catalog::{
        regrole_dependencies::{StoredRegroleConstants, StoredRegroleResolver},
        resolution::RelationLookupMode,
    },
    plan::UnifiedPlan,
    plpgsql::PlpgsqlCatalog,
    type_resolution::canonical_routine_type_name,
    SQLError, ScalarExpr,
};

pub trait RoutineParserCatalog {
    fn plpgsql_catalog(&self) -> Result<PlpgsqlCatalog, SQLError>;
    fn parser_settings(&self) -> crate::parser::ParserSettings {
        crate::parser::ParserSettings::default()
    }
    fn parser_notice(&self, _notice: crate::SQLNotice) {}
}
pub trait RoutineCompilationCatalog: crate::schema::dependencies::oid_alias::OidAliasInput {
    fn has_registered_aggregate_function(&self, name: &str) -> bool;
    fn binding_snapshot(&self) -> Result<BindingSnapshot, SQLError>;
    fn stored_query_namespace(&self) -> StoredQueryNamespace;
    /// External adapters retain their ordinary callback boundary; native implementations construct each captured name under this control.
    fn binding_snapshot_with_control(
        &self,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<crate::binding::production::ControlledBindingSnapshot, SQLError> {
        control.check()?;
        self.binding_snapshot()?.copy_with_control(control)
    }
    fn stored_query_namespace_with_control(
        &self,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<crate::binding::production::ControlledStoredQueryNamespace, SQLError> {
        control.check()?;
        self.stored_query_namespace().copy_with_control(control)
    }
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
    compile_function_body_inner(
        context,
        def,
        false,
        crate::plpgsql::PLpgSQLCompileMode::Validate,
    )
}

/// Compile executable procedural structure without validating unreached SQL fragments.
pub fn compile_function_body_for_execution(
    context: &RoutineCompilationContext<'_>,
    def: &CreateFunction,
) -> Result<CompiledFunctionBody, SQLError> {
    compile_function_body_inner(
        context,
        def,
        false,
        crate::plpgsql::PLpgSQLCompileMode::Runtime,
    )
}

pub fn compile_persisted_function_body(
    context: &RoutineCompilationContext<'_>,
    def: &CreateFunction,
) -> Result<CompiledFunctionBody, SQLError> {
    compile_function_body_inner(
        context,
        def,
        true,
        crate::plpgsql::PLpgSQLCompileMode::Validate,
    )
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
pub(super) fn validate_routine_signature(
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
    mode: crate::plpgsql::PLpgSQLCompileMode,
) -> Result<CompiledFunctionBody, SQLError> {
    with_parser_context(context.parsers, || {
        compile_body(context, def, persisted_definition, mode)
    })
}

/// Read live lexical settings only after the routine's own configuration has been applied, and deliver parser notices even if compilation fails.
pub fn with_parser_context<T>(
    parsers: &dyn RoutineParserCatalog,
    compile: impl FnOnce() -> Result<T, SQLError>,
) -> Result<T, SQLError> {
    let (result, metadata) = crate::parser::with_settings(parsers.parser_settings(), compile);
    for notice in metadata.notices.iter() {
        parsers.parser_notice(notice.clone());
    }
    result
}

fn compile_body(
    context: &RoutineCompilationContext<'_>,
    def: &CreateFunction,
    persisted_definition: bool,
    mode: crate::plpgsql::PLpgSQLCompileMode,
) -> Result<CompiledFunctionBody, SQLError> {
    let mut stored_regrole_constants = validate_routine_signature(context, def)?;
    match def.language.as_str() {
        "plpgsql" => {
            stored_regrole_constants.reject_with(context.regroles)?;
            reject_trigger_function_arguments(def)?;
            let catalog = context.parsers.plpgsql_catalog()?;
            let mut function =
                crate::plpgsql::parse_function_with_catalog_mode(def, &catalog, mode)?;
            resolve_plpgsql_datum_types(context.types, &mut function)?;
            Ok(CompiledFunctionBody::PLpgSQL(function))
        }
        "sql" => {
            let (statements, bind_catalog_dependencies) = match &def.body {
                FunctionBody::Source(source) => {
                    stored_regrole_constants.reject_with(context.regroles)?;
                    (
                        crate::compile(source).map_err(|error| def.source_body_error(error))?,
                        false,
                    )
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
            // A SQL-standard body keeps definition-time names. Source syntax
            // stays unbound so statement reanalysis can use the current path;
            // successful first-use analysis retains its own type identities.
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
            let ScalarExpr::Func {
                name,
                binding,
                args,
                ..
            } = expression
            else {
                return;
            };
            crate::ast::FunctionBinding::upgrade_legacy_serialized_dispatch(name, binding);
            crate::ast::FunctionBinding::upgrade_legacy_array_slices(
                binding,
                args.len(),
                |index| matches!(args[index], ScalarExpr::Literal(uqa_core::Value::Null)),
            );
        });
    }
    if lowering.bind_catalog_dependencies {
        match &mut plan {
            UnifiedPlan::Query(query) => {
                let namespace = context.catalog.stored_query_namespace();
                stored_relations::bind_stored_query_relations(
                    &StoredQueryBindingContext {
                        relations: context.relations,
                        lookup_mode: if lowering.persisted_definition {
                            RelationLookupMode::Bound
                        } else {
                            RelationLookupMode::Dynamic
                        },
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

/// SQL-standard bodies retain analyzed queries and DML, never utility commands. Use the same command name as ordinary SQL diagnostics.
pub(super) fn validate_sql_standard_statement(statement: &Statement) -> Result<(), SQLError> {
    match sql_standard_utility(statement) {
        Some(command) => Err(sql_standard_utility_error(&command)),
        None => Ok(()),
    }
}

fn sql_standard_utility(statement: &Statement) -> Option<Box<crate::plan::CommandPlan>> {
    match statement {
        Statement::Select(_)
        | Statement::Values { .. }
        | Statement::Insert(_)
        | Statement::Update(_)
        | Statement::Delete(_)
        | Statement::Merge(_) => None,
        _ => match UnifiedPlan::lower(statement.clone()) {
            UnifiedPlan::Command(command) => Some(command),
            UnifiedPlan::Query(_) => None,
        },
    }
}

fn sql_standard_utility_error(command: &crate::plan::CommandPlan) -> SQLError {
    SQLError::Unsupported(format!(
        "{} is not yet supported in unquoted SQL function body",
        crate::result::completion::command_tag_name(command)
    ))
}

/// `PostgreSQL` transforms the query inputs of CTAS, EXPLAIN and cursors, and resolves CALL, before rejecting the resulting utility. Other utility targets remain unexamined.
pub(super) fn validate_sql_standard_statement_inputs(
    context: &RoutineCompilationContext<'_>,
    overloads: &super::resolution::RoutineOverloadContext<'_>,
    statement: &Statement,
    params: &[crate::SQLParam],
    parameters: &crate::binding::RoutineParameterScope,
) -> Result<(), SQLError> {
    use crate::plan::CommandPlan;
    let Some(command) = sql_standard_utility(statement) else {
        return Ok(());
    };
    let error = sql_standard_utility_error(&command);
    let plan = match *command {
        CommandPlan::Explain { body, .. } => *body,
        CommandPlan::CreateTableAs { query, .. }
        | CommandPlan::CreateMaterializedView { query, .. }
        | CommandPlan::DeclareCursor { query, .. } => UnifiedPlan::Query(query),
        command @ CommandPlan::Call { .. } => UnifiedPlan::Command(Box::new(command)),
        _ => return Err(error),
    };
    analyze_sql_standard_utility_input(context, overloads, plan, params, parameters)?;
    Err(error)
}

fn analyze_sql_standard_utility_input(
    context: &RoutineCompilationContext<'_>,
    overloads: &super::resolution::RoutineOverloadContext<'_>,
    mut plan: UnifiedPlan,
    params: &[crate::SQLParam],
    parameters: &crate::binding::RoutineParameterScope,
) -> Result<(), SQLError> {
    use crate::plan::CommandPlan;
    if let UnifiedPlan::Command(command) = plan {
        match *command {
            CommandPlan::Explain { body, .. } => {
                return analyze_sql_standard_utility_input(
                    context, overloads, *body, params, parameters,
                )
            }
            CommandPlan::CreateTableAs { query, .. }
            | CommandPlan::CreateMaterializedView { query, .. }
            | CommandPlan::DeclareCursor { query, .. } => plan = UnifiedPlan::Query(query),
            command => plan = UnifiedPlan::Command(Box::new(command)),
        }
    }
    let binding = context.catalog.binding_snapshot()?;
    if matches!(&plan, UnifiedPlan::Command(command) if matches!(command.as_ref(), CommandPlan::Call { .. }))
    {
        crate::binding::bind_routine_parameter_references(
            context.routines,
            &mut plan,
            params,
            &binding.context(),
            parameters,
        )?;
        super::body_validation::analyze_body_statement(
            &super::body_validation::SQLBodyValidationContext {
                compilation: *context,
                overloads: super::resolution::RoutineOverloadContext {
                    catalog: overloads.catalog,
                },
            },
            &plan,
            params,
            &binding,
            false,
        )?;
    } else {
        crate::binding::analyze_routine_body_inputs(
            context.routines,
            &plan,
            params,
            &binding.context(),
            context.catalog,
            Some(parameters),
        )?;
    }
    Ok(())
}

/// A successfully analyzed body keeps resolved type identities independently of
/// its raw source. Domain coercions still resolve current constraint definitions.
pub fn bind_analyzed_sql_body_types(
    types: &dyn RoutineTypeCatalog,
    plan: &mut UnifiedPlan,
) -> Result<(), SQLError> {
    crate::binding::stored_types::bind_unified_plan_type_identities(plan, &mut |name| {
        let resolved = types.resolve_catalog_column_type(name);
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
