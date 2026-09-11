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
    routine_local_name, CompiledFunctionBody,
};
use crate::{
    ast::{ColumnType, CreateFunction, FunctionBody, Statement},
    catalog::regrole_dependencies::StoredRegroleResolver,
    plan::{QueryPlan, UnifiedPlan},
    plpgsql::PlpgsqlCatalog,
    RowSchema, SQLError, SQLParam, ScalarExpr,
};

pub trait RoutineParserCatalog {
    fn plpgsql_catalog(&self) -> Result<PlpgsqlCatalog, SQLError>;
}
pub trait RoutinePlanBinding {
    fn has_registered_aggregate_function(&self, name: &str) -> bool;
    fn bind_definition_query_relations(&self, query: &mut QueryPlan) -> Result<(), SQLError>;
    fn bind_persisted_query_relations(&self, query: &mut QueryPlan) -> Result<(), SQLError>;
    fn bind_query_routines(
        &self,
        query: &mut QueryPlan,
        params: &[SQLParam],
        outer: &RowSchema,
    ) -> Result<RowSchema, SQLError>;
}
pub struct RoutineCompilationContext<'a> {
    pub types: &'a dyn RoutineTypeCatalog,
    pub parsers: &'a dyn RoutineParserCatalog,
    pub bindings: &'a dyn RoutinePlanBinding,
    pub merge: &'a dyn StoredMergeColumnCatalog,
    pub regroles: &'a dyn StoredRegroleResolver,
}

pub fn compile_function_body(
    context: &RoutineCompilationContext<'_>,
    def: &CreateFunction,
) -> Result<CompiledFunctionBody, SQLError> {
    compile_function_body_inner(context, def, false, false)
}

pub fn compile_persisted_function_body(
    context: &RoutineCompilationContext<'_>,
    def: &CreateFunction,
) -> Result<CompiledFunctionBody, SQLError> {
    compile_function_body_inner(context, def, true, false)
}

pub fn compile_persisted_function_dependencies(
    context: &RoutineCompilationContext<'_>,
    def: &CreateFunction,
) -> Result<CompiledFunctionBody, SQLError> {
    compile_function_body_inner(context, def, true, true)
}

fn compile_function_body_inner(
    context: &RoutineCompilationContext<'_>,
    def: &CreateFunction,
    persisted_definition: bool,
    preserve_target_expressions: bool,
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
                preserve_target_expressions,
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
    preserve_target_expressions: bool,
) -> Result<Vec<UnifiedPlan>, SQLError> {
    let local_name = routine_local_name(&def.name)?;
    let signature_params = def.signature_params();
    let parameter_names: Vec<String> = signature_params
        .iter()
        .map(|parameter| parameter.name.clone())
        .collect();
    let parameter_types = signature_params
        .iter()
        .map(|parameter| {
            context
                .types
                .resolve_catalog_column_type(&parameter.type_name)
                .or_else(|| ColumnType::from_sql_name(&parameter.type_name).ok())
        })
        .collect::<Vec<_>>();
    let positional_parameters = parameter_types
        .iter()
        .map(|parameter_type| match parameter_type {
            Some(parameter_type) => {
                crate::SQLParam::typed_scalar(uqa_core::Value::Null, parameter_type.clone())
            }
            None => crate::SQLParam::scalar(uqa_core::Value::Null),
        })
        .collect::<Vec<_>>();
    let parameter_scope = crate::RowSchema::with_qualified_types(
        &local_name,
        parameter_names.clone(),
        parameter_types,
    );
    statements
        .into_iter()
        .map(|mut statement| {
            if bind_catalog_dependencies && !preserve_target_expressions {
                super::merge_columns::normalize_stored_merge_target_columns(
                    context.merge,
                    &mut statement,
                )?;
            }
            let mut plan = UnifiedPlan::lower_with(statement, &|name: &str| {
                context.bindings.has_registered_aggregate_function(name)
            });
            if persisted_definition {
                plan.rewrite_scalar_expressions(&mut |expression| {
                    let ScalarExpr::Func { name, binding, .. } = expression else {
                        return;
                    };
                    crate::ast::FunctionBinding::upgrade_legacy_serialized_dispatch(name, binding);
                });
            }
            if bind_catalog_dependencies {
                match &mut plan {
                    UnifiedPlan::Query(query) => {
                        if persisted_definition {
                            context.bindings.bind_persisted_query_relations(query)?;
                        } else {
                            context.bindings.bind_definition_query_relations(query)?;
                        }
                        context.bindings.bind_query_routines(
                            query,
                            &positional_parameters,
                            &parameter_scope,
                        )?;
                    }
                    UnifiedPlan::Command(_) => {
                        crate::binding::stored_routines::mark_catalog_statement_relations_bound(
                            &mut plan,
                        )?;
                    }
                }
            }
            plan.rewrite_scalar_expressions(&mut |expression| {
                let parameter = match expression {
                    ScalarExpr::Column(name) => parameter_names
                        .iter()
                        .position(|parameter| !parameter.is_empty() && parameter == name),
                    ScalarExpr::QualifiedColumn {
                        qualifier, column, ..
                    } if qualifier == &local_name => parameter_names
                        .iter()
                        .position(|parameter| !parameter.is_empty() && parameter == column),
                    _ => None,
                };
                if let Some(position) = parameter {
                    *expression = ScalarExpr::Param(position + 1);
                }
            });
            // Stored definitions retain their analyzed logical expressions;
            // immutable evaluation belongs to invocation planning.
            Ok(plan)
        })
        .collect()
}
