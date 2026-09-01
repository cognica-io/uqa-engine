//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{builtin_function_dispatch_name, BTreeSet, Engine, SQLError};

mod routines;

use routines::plpgsql_function_may_mutate_engine;

#[derive(Clone, Copy)]
struct MutabilityClassification {
    include_session_mutations: bool,
    procedural_state_requires_transaction: bool,
}

impl MutabilityClassification {
    const DATABASE_WRITES: Self = Self {
        include_session_mutations: false,
        procedural_state_requires_transaction: false,
    };
    const ENGINE_MUTATIONS: Self = Self {
        include_session_mutations: true,
        procedural_state_requires_transaction: false,
    };
    const STATEMENT_TRANSACTION: Self = Self {
        include_session_mutations: true,
        procedural_state_requires_transaction: true,
    };
}

/// SELECT is not synonymous with read-only: UQA exposes a small set of state-changing scalar functions, and SQL/PLpgSQL routines invoked from a projection can contain commands. Classify those plans before choosing the transaction mode so memory execution takes a rollback snapshot and `SQLite` opens a write transaction. Cloning the plan is bounded by query size and avoids the database-sized deep copy paid by a full memory snapshot.
pub(super) fn query_may_mutate_engine(
    engine: &Engine,
    query: &uqa_planner::QueryPlan,
) -> Result<bool, SQLError> {
    query_may_mutate_engine_inner(
        engine,
        query,
        &mut BTreeSet::new(),
        &mut BTreeSet::new(),
        MutabilityClassification::ENGINE_MUTATIONS,
    )
}

/// Some read-only operations still need a statement transaction. In particular, a PL/pgSQL block with an `EXCEPTION` arm opens a subtransaction even when neither branch writes data.
pub(super) fn query_requires_statement_transaction(
    engine: &Engine,
    query: &uqa_planner::QueryPlan,
) -> Result<bool, SQLError> {
    query_may_mutate_engine_inner(
        engine,
        query,
        &mut BTreeSet::new(),
        &mut BTreeSet::new(),
        MutabilityClassification::STATEMENT_TRANSACTION,
    )
}

/// Classify only database writes forbidden by `PostgreSQL` read-only transactions. Session-local effects such as `random()` and `setseed()` still require statement rollback bookkeeping but remain legal in read-only mode.
pub(super) fn query_may_write_database(
    engine: &Engine,
    query: &uqa_planner::QueryPlan,
) -> Result<bool, SQLError> {
    query_may_mutate_engine_inner(
        engine,
        query,
        &mut BTreeSet::new(),
        &mut BTreeSet::new(),
        MutabilityClassification::DATABASE_WRITES,
    )
}

/// Detect database-writing expressions and query sources embedded in an otherwise legal temporary-table DML command.
pub(super) fn command_payload_may_write_database(
    engine: &Engine,
    command: &uqa_planner::CommandPlan,
) -> Result<bool, SQLError> {
    let mut plan = uqa_planner::UnifiedPlan::Command(Box::new(command.clone()));
    let mut writes = false;
    let mut classification_error = None;
    plan.rewrite_scalar_expressions(&mut |expression| {
        if classification_error.is_some() {
            return;
        }
        let uqa_execution::ScalarExpr::Func {
            name,
            binding,
            args,
            ..
        } = expression
        else {
            return;
        };
        match function_may_mutate_engine(
            engine,
            name,
            binding.as_ref(),
            args,
            &mut BTreeSet::new(),
            &mut BTreeSet::new(),
            MutabilityClassification::DATABASE_WRITES,
        ) {
            Ok(value) => writes |= value,
            Err(error) => classification_error = Some(error),
        }
    });
    if let Some(error) = classification_error {
        return Err(error);
    }
    if writes {
        return Ok(true);
    }
    match command {
        uqa_planner::CommandPlan::Insert(plan) => {
            if queries_may_write_database(engine, plan.ctes.iter().map(|cte| cte.query.as_ref()))? {
                return Ok(true);
            }
            if let Some(source) = plan.source.as_deref() {
                if query_may_write_database(engine, source)? {
                    return Ok(true);
                }
            }
            queries_may_write_database(engine, &plan.subqueries)
        }
        uqa_planner::CommandPlan::Update(plan) => {
            if queries_may_write_database(engine, plan.ctes.iter().map(|cte| cte.query.as_ref()))? {
                return Ok(true);
            }
            if let Some(source) = plan.source.as_deref() {
                if source_may_mutate_engine(
                    engine,
                    source,
                    &mut BTreeSet::new(),
                    &mut BTreeSet::new(),
                    MutabilityClassification::DATABASE_WRITES,
                )? {
                    return Ok(true);
                }
            }
            queries_may_write_database(engine, &plan.subqueries)
        }
        uqa_planner::CommandPlan::Delete(plan) => {
            if queries_may_write_database(engine, plan.ctes.iter().map(|cte| cte.query.as_ref()))? {
                return Ok(true);
            }
            if let Some(source) = plan.source.as_deref() {
                if source_may_mutate_engine(
                    engine,
                    source,
                    &mut BTreeSet::new(),
                    &mut BTreeSet::new(),
                    MutabilityClassification::DATABASE_WRITES,
                )? {
                    return Ok(true);
                }
            }
            queries_may_write_database(engine, &plan.subqueries)
        }
        uqa_planner::CommandPlan::Merge(plan) => {
            if source_may_mutate_engine(
                engine,
                &plan.source,
                &mut BTreeSet::new(),
                &mut BTreeSet::new(),
                MutabilityClassification::DATABASE_WRITES,
            )? {
                return Ok(true);
            }
            queries_may_write_database(engine, &plan.subqueries)
        }
        _ => Ok(false),
    }
}

fn queries_may_write_database<'a>(
    engine: &Engine,
    queries: impl IntoIterator<Item = &'a uqa_planner::QueryPlan>,
) -> Result<bool, SQLError> {
    for query in queries {
        if query_may_write_database(engine, query)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn query_may_mutate_engine_inner(
    engine: &Engine,
    query: &uqa_planner::QueryPlan,
    visiting_views: &mut BTreeSet<String>,
    visiting_routines: &mut BTreeSet<String>,
    classification: MutabilityClassification,
) -> Result<bool, SQLError> {
    if query_source_may_mutate_engine(
        engine,
        query,
        visiting_views,
        visiting_routines,
        classification,
    )? {
        return Ok(true);
    }
    let mut plan = uqa_planner::UnifiedPlan::Query(Box::new(query.clone()));
    let mut mutates = uqa_planner::optimizer::query_contains_implicit_hybrid_fusion(query);
    let mut classification_error = None;
    plan.rewrite_scalar_expressions(&mut |expression| {
        if classification_error.is_some() {
            return;
        }
        let uqa_execution::ScalarExpr::Func {
            name,
            binding,
            args,
            ..
        } = expression
        else {
            return;
        };
        match function_may_mutate_engine(
            engine,
            name,
            binding.as_ref(),
            args,
            visiting_views,
            visiting_routines,
            classification,
        ) {
            Ok(value) => mutates |= value,
            Err(error) => classification_error = Some(error),
        }
    });
    classification_error.map_or(Ok(mutates), Err)
}

fn function_may_mutate_engine(
    engine: &Engine,
    name: &str,
    binding: Option<&uqa_sql::ast::FunctionBinding>,
    args: &[uqa_execution::ScalarExpr],
    visiting_views: &mut BTreeSet<String>,
    visiting_routines: &mut BTreeSet<String>,
    classification: MutabilityClassification,
) -> Result<bool, SQLError> {
    let identity = name.to_ascii_lowercase();
    let dispatch_name = builtin_function_dispatch_name(&identity);
    let cypher_mutates = if dispatch_name == "cypher" {
        match args.get(1) {
            Some(uqa_execution::ScalarExpr::Literal(uqa_core::Value::Str(query))) => {
                super::age_cypher::query_is_mutating(query)?
            }
            _ => classification.include_session_mutations,
        }
    } else {
        false
    };
    let mutates_database_directly = cypher_mutates
        || matches!(
            dispatch_name.as_str(),
            "create_analyzer"
                | "drop_analyzer"
                | "set_table_analyzer"
                | "graph_create"
                | "graph_drop"
                | "create_graph"
                | "drop_graph"
                | "create_vlabel"
                | "create_elabel"
                | "drop_label"
                | "alter_graph"
                | "deep_learn"
                | "bayesian_match"
                | "bayesian_match_with_prior"
        )
        || engine.registered_runtime_function_may_mutate_engine(&identity);
    let temporary_sequence_call = matches!(dispatch_name.as_str(), "nextval" | "setval")
        && sequence_function_targets_temporary(engine, args)?;
    let builtin_requires_mutating_execution =
        matches!(
            dispatch_name.as_str(),
            "random" | "setseed" | "fts_match" | "multi_field_match"
        ) || (matches!(dispatch_name.as_str(), "nextval" | "setval") && !temporary_sequence_call);
    let sql_routine_mutates = classification.include_session_mutations
        && sql_routine_may_mutate_engine(
            engine,
            &identity,
            binding,
            visiting_views,
            visiting_routines,
            classification,
        )?;
    Ok(mutates_database_directly
        || (classification.include_session_mutations
            && (builtin_requires_mutating_execution || sql_routine_mutates)))
}

fn sequence_function_targets_temporary(
    engine: &Engine,
    args: &[uqa_execution::ScalarExpr],
) -> Result<bool, SQLError> {
    fn literal_reference(expression: &uqa_execution::ScalarExpr) -> Option<&str> {
        match expression {
            uqa_execution::ScalarExpr::Literal(uqa_core::Value::Str(reference)) => Some(reference),
            uqa_execution::ScalarExpr::Cast { expr, .. } => literal_reference(expr),
            _ => None,
        }
    }

    let Some(reference) = args.first().and_then(literal_reference) else {
        return Ok(false);
    };
    Ok(engine
        .sequence_persistence(reference)
        .map_err(|error| {
            SQLError::Internal(format!(
                "resolve sequence `{reference}` while classifying query mutability: {error}"
            ))
        })?
        .is_some_and(|persistence| persistence == uqa_sql::ast::RelationPersistence::Temporary))
}

fn sql_routine_may_mutate_engine(
    engine: &Engine,
    name: &str,
    binding: Option<&uqa_sql::ast::FunctionBinding>,
    visiting_views: &mut BTreeSet<String>,
    visiting_routines: &mut BTreeSet<String>,
    classification: MutabilityClassification,
) -> Result<bool, SQLError> {
    if binding.is_some_and(|binding| binding.builtin) {
        return Ok(false);
    }
    let lookup_name = binding.map_or(name, |binding| binding.name.as_str());
    let Some(overloads) = engine.lookup_sql_functions(lookup_name) else {
        return Ok(false);
    };
    for function in overloads {
        if function.def.is_procedure
            || binding.is_some_and(|binding| {
                crate::engine_user_functions::routine_signature_types(&function.def)
                    != binding.argument_types
            })
        {
            continue;
        }
        let signature = crate::engine_user_functions::routine_signature_types(&function.def);
        let key = format!("{}({})", function.def.name, signature.join(","));
        if !visiting_routines.insert(key.clone()) {
            continue;
        }
        let result = match &function.compiled {
            crate::engine_user_functions::CompiledFunctionBody::SQL(plans) => (|| {
                let mut mutates = false;
                for plan in plans {
                    match plan {
                        uqa_planner::UnifiedPlan::Query(query) => {
                            if query_may_mutate_engine_inner(
                                engine,
                                query,
                                visiting_views,
                                visiting_routines,
                                classification,
                            )? {
                                mutates = true;
                                break;
                            }
                        }
                        uqa_planner::UnifiedPlan::Command(_) => {
                            mutates = true;
                            break;
                        }
                    }
                }
                Ok(mutates)
            })(),
            crate::engine_user_functions::CompiledFunctionBody::PLpgSQL(function) => {
                plpgsql_function_may_mutate_engine(
                    engine,
                    function,
                    visiting_views,
                    visiting_routines,
                    classification,
                )
            }
        };
        visiting_routines.remove(&key);
        if result? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn query_source_may_mutate_engine(
    engine: &Engine,
    query: &uqa_planner::QueryPlan,
    visiting_views: &mut BTreeSet<String>,
    visiting_routines: &mut BTreeSet<String>,
    classification: MutabilityClassification,
) -> Result<bool, SQLError> {
    for cte in &query.ctes {
        if query_source_may_mutate_engine(
            engine,
            &cte.query,
            visiting_views,
            visiting_routines,
            classification,
        )? {
            return Ok(true);
        }
    }
    match &query.root {
        uqa_planner::RelationalPlan::QueryBlock(block) => {
            if let Some(source) = block.from.as_ref() {
                if source_may_mutate_engine(
                    engine,
                    source,
                    visiting_views,
                    visiting_routines,
                    classification,
                )? {
                    return Ok(true);
                }
            }
            for subquery in &block.subqueries {
                if query_source_may_mutate_engine(
                    engine,
                    subquery,
                    visiting_views,
                    visiting_routines,
                    classification,
                )? {
                    return Ok(true);
                }
            }
        }
        uqa_planner::RelationalPlan::SetOp {
            left,
            right,
            subqueries,
            ..
        } => {
            if query_source_may_mutate_engine(
                engine,
                left,
                visiting_views,
                visiting_routines,
                classification,
            )? || query_source_may_mutate_engine(
                engine,
                right,
                visiting_views,
                visiting_routines,
                classification,
            )? {
                return Ok(true);
            }
            for subquery in subqueries {
                if query_source_may_mutate_engine(
                    engine,
                    subquery,
                    visiting_views,
                    visiting_routines,
                    classification,
                )? {
                    return Ok(true);
                }
            }
        }
        uqa_planner::RelationalPlan::Values { subqueries, .. } => {
            for subquery in subqueries {
                if query_source_may_mutate_engine(
                    engine,
                    subquery,
                    visiting_views,
                    visiting_routines,
                    classification,
                )? {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

fn source_may_mutate_engine(
    engine: &Engine,
    source: &uqa_planner::SourcePlan,
    visiting_views: &mut BTreeSet<String>,
    visiting_routines: &mut BTreeSet<String>,
    classification: MutabilityClassification,
) -> Result<bool, SQLError> {
    match source {
        uqa_planner::SourcePlan::Function {
            name,
            binding,
            args,
            ..
        } => function_may_mutate_engine(
            engine,
            name,
            binding.as_ref(),
            args,
            visiting_views,
            visiting_routines,
            classification,
        ),
        uqa_planner::SourcePlan::FunctionGroup { functions, .. } => {
            for function in functions {
                if function_may_mutate_engine(
                    engine,
                    &function.name,
                    function.binding.as_ref(),
                    &function.args,
                    visiting_views,
                    visiting_routines,
                    classification,
                )? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        uqa_planner::SourcePlan::Join { left, right, .. } => Ok(source_may_mutate_engine(
            engine,
            left,
            visiting_views,
            visiting_routines,
            classification,
        )? || source_may_mutate_engine(
            engine,
            right,
            visiting_views,
            visiting_routines,
            classification,
        )?),
        uqa_planner::SourcePlan::Subquery { body, .. } => query_source_may_mutate_engine(
            engine,
            body,
            visiting_views,
            visiting_routines,
            classification,
        ),
        uqa_planner::SourcePlan::Table { name, .. } => {
            let key = name.to_ascii_lowercase();
            if !visiting_views.insert(key.clone()) {
                return Ok(false);
            }
            let result = match engine.view_plan(name) {
                Ok(Some(view)) => query_may_mutate_engine_inner(
                    engine,
                    &view,
                    visiting_views,
                    visiting_routines,
                    classification,
                ),
                Ok(None) => Ok(false),
                Err(error) => Err(error),
            };
            visiting_views.remove(&key);
            result
        }
        uqa_planner::SourcePlan::Values { .. } => Ok(false),
    }
}

pub(super) fn is_transaction_control(plan: &uqa_planner::UnifiedPlan) -> bool {
    matches!(
        plan,
        uqa_planner::UnifiedPlan::Command(command)
            if matches!(command.as_ref(), uqa_planner::CommandPlan::Transaction(_))
    )
}
