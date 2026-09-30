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
    routine_local_name, CompiledFunctionBody, RoutineResolution,
};
use crate::{
    ast::{ColumnType, CreateFunction, FunctionBody, Statement},
    binding::{
        snapshot::BindingSnapshot,
        stored_relations::{
            self, StoredQueryBindingContext, StoredQueryNamespace, StoredQuerySequences,
            StoredRelationCatalog,
        },
    },
    catalog::regrole_dependencies::StoredRegroleResolver,
    plan::UnifiedPlan,
    plpgsql::PlpgsqlCatalog,
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

fn compile_function_body_inner(
    context: &RoutineCompilationContext<'_>,
    def: &CreateFunction,
    persisted_definition: bool,
) -> Result<CompiledFunctionBody, SQLError> {
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
    let mut stored_regrole_constants = routine_parameter_regrole_constants(context.types, def);
    stored_regrole_constants.validate_inputs_with(context.regroles)?;
    validate_routine_declaration(context.types, def)?;
    match def.language.as_str() {
        "plpgsql" => {
            stored_regrole_constants.reject_with(context.regroles)?;
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

/// The parameters of a SQL routine body: their names, typed positional values for binding, and the row scope that resolves them by name.
pub struct SQLRoutineParameters {
    local_name: String,
    names: Vec<String>,
    pub positional: Vec<crate::SQLParam>,
    pub scope: crate::RowSchema,
}

pub fn sql_routine_parameters(
    context: &RoutineCompilationContext<'_>,
    def: &CreateFunction,
) -> Result<SQLRoutineParameters, SQLError> {
    let local_name = routine_local_name(&def.name)?;
    let signature_params = def.signature_params();
    let names: Vec<String> = signature_params
        .iter()
        .map(|parameter| parameter.name.clone())
        .collect();
    let types = signature_params
        .iter()
        .map(|parameter| {
            context
                .types
                .resolve_catalog_column_type(&parameter.type_name)
                .or_else(|| ColumnType::from_sql_name(&parameter.type_name).ok())
        })
        .collect::<Vec<_>>();
    let positional = types
        .iter()
        .map(|parameter_type| match parameter_type {
            Some(parameter_type) => {
                crate::SQLParam::typed_scalar(uqa_core::Value::Null, parameter_type.clone())
            }
            None => crate::SQLParam::scalar(uqa_core::Value::Null),
        })
        .collect::<Vec<_>>();
    let scope = crate::RowSchema::with_qualified_types(&local_name, names.clone(), types);
    Ok(SQLRoutineParameters {
        local_name,
        names,
        positional,
        scope,
    })
}

impl SQLRoutineParameters {
    /// Replace references to the routine's parameters by positional parameters, as the body is invoked.
    pub fn bind_references(&self, plan: &mut UnifiedPlan) {
        plan.rewrite_scalar_expressions(&mut |expression| {
            let parameter = match expression {
                ScalarExpr::Column(name) => self
                    .names
                    .iter()
                    .position(|parameter| !parameter.is_empty() && parameter == name),
                ScalarExpr::QualifiedColumn {
                    qualifier, column, ..
                } if qualifier == &self.local_name => self
                    .names
                    .iter()
                    .position(|parameter| !parameter.is_empty() && parameter == column),
                _ => None,
            };
            if let Some(position) = parameter {
                *expression = ScalarExpr::Param(position + 1);
            }
        });
    }
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

fn compile_sql_routine_plans(
    context: &RoutineCompilationContext<'_>,
    def: &CreateFunction,
    statements: Vec<Statement>,
    bind_catalog_dependencies: bool,
    persisted_definition: bool,
) -> Result<Vec<UnifiedPlan>, SQLError> {
    let parameters = sql_routine_parameters(context, def)?;
    let lowering = SQLRoutineLowering {
        bind_catalog_dependencies,
        persisted_definition,
        preserve_target_expressions: false,
    };
    statements
        .into_iter()
        .map(|statement| {
            let mut plan = lower_sql_routine_statement(context, statement, lowering)?;
            if let (true, UnifiedPlan::Query(query)) = (bind_catalog_dependencies, &mut plan) {
                let binding = context.catalog.binding_snapshot()?;
                crate::binding::bind_query_plan_routines_for_storage(
                    context.routines,
                    query,
                    &parameters.positional,
                    &binding.context(),
                    Some(&parameters.scope),
                )?;
            } else if !bind_catalog_dependencies {
                bind_session_plan_types(context, &mut plan)?;
            }
            parameters.bind_references(&mut plan);
            // Stored definitions retain their analyzed logical expressions;
            // immutable evaluation belongs to invocation planning.
            Ok(plan)
        })
        .collect()
}
