//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact routine binding for catalog-owned statements.

use uqa_execution::{RowSchema, ScalarExpr, ScalarFrameBound};
use uqa_planner::{
    AccessPathPlan, CommandPlan, ComputePlan, ConflictActionPlan, CtePlan, DeletePlan, InsertPlan,
    JoinExecutionStrategy, MergePlan, ProjectionPlan, QueryBlockPlan, QueryPlan, RelationalPlan,
    SourcePlan, UnifiedPlan, UpdatePlan,
};
use uqa_sql::ast::FunctionBinding;
use uqa_sql::SQLError;

use super::{Engine, Value};

pub(crate) struct BoundStatementRoutines {
    pub(crate) query: Option<QueryPlan>,
    pub(crate) references: Vec<BoundRoutineReference>,
}

#[derive(Debug, Clone)]
pub(crate) struct BoundRoutineReference {
    pub(crate) name: String,
    pub(crate) binding: Option<FunctionBinding>,
}

struct CommandRoutineInputs {
    ctes: Vec<CtePlan>,
    source: Option<SourcePlan>,
    expressions: Vec<ScalarExpr>,
    subqueries: Vec<QueryPlan>,
    outer: RowSchema,
}

pub(crate) fn bind_catalog_statement_routines(
    engine: &Engine,
    plan: &UnifiedPlan,
) -> Result<BoundStatementRoutines, SQLError> {
    let query = match plan {
        UnifiedPlan::Query(query) => {
            let mut query = (**query).clone();
            mark_query_relations_bound(&mut query);
            super::bind_catalog_query_routines(engine, &mut query, &[])?;
            Some(query)
        }
        UnifiedPlan::Command(command) => bind_command_statement_routines(engine, command)?,
    };
    let mut references = Vec::new();
    if let Some(query) = &query {
        collect_query_routine_references(query, &mut references)?;
    }
    Ok(BoundStatementRoutines { query, references })
}

pub(crate) fn mark_catalog_statement_relations_bound(
    plan: &mut UnifiedPlan,
) -> Result<(), SQLError> {
    match plan {
        UnifiedPlan::Query(query) => mark_query_relations_bound(query),
        UnifiedPlan::Command(command) => match command.as_mut() {
            CommandPlan::Insert(plan) => {
                plan.relations_bound = true;
                for cte in &mut plan.ctes {
                    mark_query_relations_bound(&mut cte.query);
                }
                if let Some(source) = &mut plan.source {
                    mark_query_relations_bound(source);
                }
                for subquery in &mut plan.subqueries {
                    mark_query_relations_bound(subquery);
                }
            }
            CommandPlan::Update(plan) => {
                plan.relations_bound = true;
                for cte in &mut plan.ctes {
                    mark_query_relations_bound(&mut cte.query);
                }
                if let Some(source) = &mut plan.source {
                    mark_source_relations_bound(source);
                }
                for subquery in &mut plan.subqueries {
                    mark_query_relations_bound(subquery);
                }
            }
            CommandPlan::Delete(plan) => {
                plan.relations_bound = true;
                for cte in &mut plan.ctes {
                    mark_query_relations_bound(&mut cte.query);
                }
                if let Some(source) = &mut plan.source {
                    mark_source_relations_bound(source);
                }
                for subquery in &mut plan.subqueries {
                    mark_query_relations_bound(subquery);
                }
            }
            CommandPlan::Notify { .. } => {}
            CommandPlan::Merge(plan) => {
                mark_source_relations_bound(&mut plan.source);
                for subquery in &mut plan.subqueries {
                    mark_query_relations_bound(subquery);
                }
            }
            _ => {
                return Err(SQLError::Internal(
                    "catalog-owned statement lowered to an unsupported command".into(),
                ));
            }
        },
    }
    Ok(())
}

fn bind_command_statement_routines(
    engine: &Engine,
    command: &CommandPlan,
) -> Result<Option<QueryPlan>, SQLError> {
    let Some(inputs) = command_statement_routine_inputs(engine, command)? else {
        return Ok(None);
    };
    let projections = inputs
        .expressions
        .into_iter()
        .filter(|expression| expression_contains_routine(expression, &inputs.subqueries))
        .filter(|expression| !matches!(expression, ScalarExpr::Default))
        .map(|expr| ProjectionPlan { expr, alias: None })
        .chain(std::iter::once(ProjectionPlan {
            expr: ScalarExpr::Literal(Value::Int(1)),
            alias: None,
        }))
        .collect();
    let mut query = QueryPlan {
        relations_bound: true,
        ctes: inputs.ctes,
        root: RelationalPlan::QueryBlock(Box::new(QueryBlockPlan {
            projections,
            from: inputs.source,
            r#where: None,
            compute: ComputePlan::Project,
            group_by: Vec::new(),
            grouping_sets: Vec::new(),
            group_distinct: false,
            having: None,
            order_by: Vec::new(),
            limit: None,
            with_ties: false,
            offset: None,
            distinct: false,
            distinct_on: Vec::new(),
            subqueries: inputs.subqueries,
            access: AccessPathPlan::Row,
            locking: Vec::new(),
        })),
    };
    mark_query_relations_bound(&mut query);
    super::bind_catalog_query_routines_with_outer(engine, &mut query, &[], &inputs.outer)?;
    Ok(Some(query))
}

fn command_statement_routine_inputs(
    engine: &Engine,
    command: &CommandPlan,
) -> Result<Option<CommandRoutineInputs>, SQLError> {
    match command {
        CommandPlan::Insert(plan) => insert_statement_routine_inputs(engine, plan).map(Some),
        CommandPlan::Update(plan) => update_statement_routine_inputs(engine, plan).map(Some),
        CommandPlan::Delete(plan) => delete_statement_routine_inputs(engine, plan).map(Some),
        CommandPlan::Merge(plan) => Ok(Some(merge_statement_routine_inputs(plan))),
        CommandPlan::Notify { .. } => Ok(None),
        _ => Err(SQLError::Internal(
            "catalog-owned statement lowered to an unsupported command".into(),
        )),
    }
}

fn merge_statement_routine_inputs(plan: &MergePlan) -> CommandRoutineInputs {
    let target = SourcePlan::Table {
        name: plan.target.clone(),
        qualifier: plan.target_qualifier.clone(),
        alias: plan.target_alias.clone(),
        column_aliases: Vec::new(),
        include_descendants: plan.include_descendants,
    };
    let source = SourcePlan::Join {
        left: Box::new(target),
        right: plan.source.clone(),
        kind: uqa_sql::ast::JoinKind::Cross,
        on: None,
        using: None,
        natural: false,
        alias: None,
        column_aliases: Vec::new(),
        lateral: false,
        strategy: JoinExecutionStrategy::default(),
    };
    let mut expressions = vec![plan.join_condition.clone()];
    for clause in &plan.when_clauses {
        match clause {
            uqa_planner::MergeWhenPlan::UpdateMatched {
                condition,
                assignments,
            }
            | uqa_planner::MergeWhenPlan::UpdateNotMatchedBySource {
                condition,
                assignments,
            } => {
                expressions.extend(condition.iter().cloned());
                expressions.extend(
                    assignments
                        .iter()
                        .map(|assignment| assignment.value.clone()),
                );
            }
            uqa_planner::MergeWhenPlan::InsertNotMatched {
                condition, values, ..
            } => {
                expressions.extend(condition.iter().cloned());
                expressions.extend(values.iter().cloned());
            }
            uqa_planner::MergeWhenPlan::DeleteMatched { condition }
            | uqa_planner::MergeWhenPlan::DeleteNotMatchedBySource { condition }
            | uqa_planner::MergeWhenPlan::NothingMatched { condition }
            | uqa_planner::MergeWhenPlan::NothingNotMatched { condition }
            | uqa_planner::MergeWhenPlan::NothingNotMatchedBySource { condition } => {
                expressions.extend(condition.iter().cloned());
            }
        }
    }
    expressions.extend(
        plan.returning
            .iter()
            .map(|projection| projection.expr.clone()),
    );
    CommandRoutineInputs {
        ctes: Vec::new(),
        source: Some(source),
        expressions,
        subqueries: plan.subqueries.clone(),
        outer: RowSchema::default(),
    }
}

fn insert_statement_routine_inputs(
    engine: &Engine,
    plan: &InsertPlan,
) -> Result<CommandRoutineInputs, SQLError> {
    let mut expressions = plan.rows.iter().flatten().cloned().collect::<Vec<_>>();
    if let Some(conflict) = &plan.on_conflict {
        expressions.extend(conflict.predicate.iter().map(Box::as_ref).cloned());
        if let ConflictActionPlan::Update {
            assignments,
            predicate,
        } = &conflict.action
        {
            expressions.extend(
                assignments
                    .iter()
                    .map(|assignment| assignment.value.clone()),
            );
            expressions.extend(predicate.iter().map(Box::as_ref).cloned());
        }
    }
    expressions.extend(
        plan.returning
            .iter()
            .map(|projection| projection.expr.clone()),
    );
    let source = plan.source.as_ref().map(|source| SourcePlan::Subquery {
        body: Box::new((**source).clone()),
        alias: Some("__uqa_catalog_statement_source".into()),
        column_aliases: Vec::new(),
    });
    Ok(CommandRoutineInputs {
        ctes: plan.ctes.clone(),
        source,
        expressions,
        subqueries: plan.subqueries.clone(),
        outer: statement_target_outer_schema(
            engine,
            &plan.table,
            &plan.target_qualifier,
            &plan.returning_aliases,
        )?,
    })
}

fn update_statement_routine_inputs(
    engine: &Engine,
    plan: &UpdatePlan,
) -> Result<CommandRoutineInputs, SQLError> {
    let mut expressions = plan
        .assignments
        .iter()
        .map(|assignment| assignment.value.clone())
        .collect::<Vec<_>>();
    expressions.extend(plan.predicate.iter().cloned());
    expressions.extend(
        plan.returning
            .iter()
            .map(|projection| projection.expr.clone()),
    );
    Ok(CommandRoutineInputs {
        ctes: plan.ctes.clone(),
        source: plan.source.as_deref().cloned(),
        expressions,
        subqueries: plan.subqueries.clone(),
        outer: statement_target_outer_schema(
            engine,
            &plan.table,
            &plan.target_qualifier,
            &plan.returning_aliases,
        )?,
    })
}

fn delete_statement_routine_inputs(
    engine: &Engine,
    plan: &DeletePlan,
) -> Result<CommandRoutineInputs, SQLError> {
    let mut expressions = plan.predicate.iter().cloned().collect::<Vec<_>>();
    expressions.extend(
        plan.returning
            .iter()
            .map(|projection| projection.expr.clone()),
    );
    Ok(CommandRoutineInputs {
        ctes: plan.ctes.clone(),
        source: plan.source.as_deref().cloned(),
        expressions,
        subqueries: plan.subqueries.clone(),
        outer: statement_target_outer_schema(
            engine,
            &plan.table,
            &plan.target_qualifier,
            &plan.returning_aliases,
        )?,
    })
}

fn expression_contains_routine(expression: &ScalarExpr, subqueries: &[QueryPlan]) -> bool {
    let mut references = Vec::new();
    collect_scalar_routine_references(expression, subqueries, &mut references).is_ok()
        && !references.is_empty()
}

fn statement_target_outer_schema(
    engine: &Engine,
    table: &str,
    target_qualifier: &str,
    aliases: &uqa_sql::ast::ReturningAliases,
) -> Result<RowSchema, SQLError> {
    let ctes = super::CteScope::new_for_catalog_binding(engine);
    let target = super::select::analyze_source_plan_schema(
        engine,
        &SourcePlan::Table {
            name: table.to_string(),
            qualifier: target_qualifier.to_string(),
            alias: None,
            column_aliases: Vec::new(),
            include_descendants: true,
        },
        &[],
        &ctes,
        None,
    )?;
    let target = RowSchema::with_types(target.columns().to_vec(), target.column_types().to_vec());
    Ok(super::dml::returning_expression_schema(
        &target,
        target_qualifier,
        aliases,
        None,
    ))
}

pub(crate) fn collect_expression_routine_references(
    expression: &uqa_planner::ExpressionPlan,
) -> Result<Vec<BoundRoutineReference>, SQLError> {
    let mut references = Vec::new();
    collect_scalar_routine_references(&expression.scalar, &expression.subqueries, &mut references)?;
    Ok(references)
}

fn collect_query_routine_references(
    query: &QueryPlan,
    references: &mut Vec<BoundRoutineReference>,
) -> Result<(), SQLError> {
    for cte in &query.ctes {
        collect_query_routine_references(&cte.query, references)?;
        if let Some(cycle) = &cte.cycle {
            collect_scalar_routine_references(&cycle.mark_value, &[], references)?;
            collect_scalar_routine_references(&cycle.mark_default, &[], references)?;
        }
    }
    match &query.root {
        RelationalPlan::QueryBlock(block) => {
            if let Some(source) = &block.from {
                collect_source_routine_references(source, &block.subqueries, references)?;
            }
            for projection in &block.projections {
                collect_scalar_routine_references(&projection.expr, &block.subqueries, references)?;
            }
            if let Some(expression) = &block.r#where {
                collect_scalar_routine_references(expression, &block.subqueries, references)?;
            }
            for expression in &block.group_by {
                collect_scalar_routine_references(expression, &block.subqueries, references)?;
            }
            for expression in block.grouping_sets.iter().flatten() {
                collect_scalar_routine_references(expression, &block.subqueries, references)?;
            }
            if let Some(expression) = &block.having {
                collect_scalar_routine_references(expression, &block.subqueries, references)?;
            }
            for order in &block.order_by {
                collect_scalar_routine_references(&order.expr, &block.subqueries, references)?;
            }
            if let Some(expression) = &block.limit {
                collect_scalar_routine_references(expression, &block.subqueries, references)?;
            }
            if let Some(expression) = &block.offset {
                collect_scalar_routine_references(expression, &block.subqueries, references)?;
            }
            for expression in &block.distinct_on {
                collect_scalar_routine_references(expression, &block.subqueries, references)?;
            }
        }
        RelationalPlan::SetOp {
            left,
            right,
            order_by,
            limit,
            offset,
            subqueries,
            ..
        } => {
            collect_query_routine_references(left, references)?;
            collect_query_routine_references(right, references)?;
            for order in order_by {
                collect_scalar_routine_references(&order.expr, subqueries, references)?;
            }
            if let Some(expression) = limit {
                collect_scalar_routine_references(expression, subqueries, references)?;
            }
            if let Some(expression) = offset {
                collect_scalar_routine_references(expression, subqueries, references)?;
            }
        }
        RelationalPlan::Values { rows, subqueries } => {
            for expression in rows.iter().flatten() {
                collect_scalar_routine_references(expression, subqueries, references)?;
            }
        }
    }
    Ok(())
}

fn collect_source_routine_references(
    source: &SourcePlan,
    subqueries: &[QueryPlan],
    references: &mut Vec<BoundRoutineReference>,
) -> Result<(), SQLError> {
    match source {
        SourcePlan::Table { .. } => {}
        SourcePlan::Join {
            left, right, on, ..
        } => {
            collect_source_routine_references(left, subqueries, references)?;
            collect_source_routine_references(right, subqueries, references)?;
            if let Some(expression) = on {
                collect_scalar_routine_references(expression, subqueries, references)?;
            }
        }
        SourcePlan::Values { rows, .. } => {
            for expression in rows.iter().flatten() {
                collect_scalar_routine_references(expression, subqueries, references)?;
            }
        }
        SourcePlan::Function {
            name,
            binding,
            args,
            ..
        } => {
            references.push(BoundRoutineReference {
                name: name.clone(),
                binding: binding.clone(),
            });
            for expression in args {
                collect_scalar_routine_references(expression, subqueries, references)?;
            }
        }
        SourcePlan::FunctionGroup { functions, .. } => {
            for function in functions {
                references.push(BoundRoutineReference {
                    name: function.name.clone(),
                    binding: function.binding.clone(),
                });
                for expression in &function.args {
                    collect_scalar_routine_references(expression, subqueries, references)?;
                }
            }
        }
        SourcePlan::Subquery { body, .. } => {
            collect_query_routine_references(body, references)?;
        }
    }
    Ok(())
}

fn collect_scalar_routine_references(
    expression: &ScalarExpr,
    subqueries: &[QueryPlan],
    references: &mut Vec<BoundRoutineReference>,
) -> Result<(), SQLError> {
    match expression {
        ScalarExpr::Func {
            name,
            binding,
            args,
            order_by,
            filter,
            ..
        } => {
            for argument in args {
                collect_scalar_routine_references(argument, subqueries, references)?;
            }
            for order in order_by {
                collect_scalar_routine_references(&order.expr, subqueries, references)?;
            }
            if let Some(filter) = filter {
                collect_scalar_routine_references(filter, subqueries, references)?;
            }
            references.push(BoundRoutineReference {
                name: name.clone(),
                binding: binding.clone(),
            });
        }
        ScalarExpr::Array(items)
        | ScalarExpr::Row(items)
        | ScalarExpr::And(items)
        | ScalarExpr::Or(items) => collect_many_routine_references(items, subqueries, references)?,
        ScalarExpr::Binary { lhs, rhs, .. } => {
            collect_scalar_routine_references(lhs, subqueries, references)?;
            collect_scalar_routine_references(rhs, subqueries, references)?;
        }
        ScalarExpr::UnaryMinus(inner)
        | ScalarExpr::Not(inner)
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::Cast { expr: inner, .. } => {
            collect_scalar_routine_references(inner, subqueries, references)?;
        }
        ScalarExpr::Between { expr, low, high } => {
            collect_scalar_routine_references(expr, subqueries, references)?;
            collect_scalar_routine_references(low, subqueries, references)?;
            collect_scalar_routine_references(high, subqueries, references)?;
        }
        ScalarExpr::InList { expr, list, .. } => {
            collect_scalar_routine_references(expr, subqueries, references)?;
            for item in list {
                collect_scalar_routine_references(item, subqueries, references)?;
            }
        }
        ScalarExpr::WindowCall { name, args, spec } => {
            collect_window_routine_references(name, args, spec, subqueries, references)?;
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => collect_case_routine_references(
            base.as_deref(),
            when,
            else_branch.as_deref(),
            subqueries,
            references,
        )?,
        ScalarExpr::ScalarSubquery(index)
        | ScalarExpr::Exists {
            subquery: index, ..
        } => {
            let query = subqueries.get(*index).ok_or_else(|| {
                SQLError::Internal(format!(
                    "stored catalog routine binding cannot resolve subquery slot {index}"
                ))
            })?;
            collect_query_routine_references(query, references)?;
        }
        ScalarExpr::InSubquery {
            expr,
            subquery: index,
            ..
        } => {
            collect_scalar_routine_references(expr, subqueries, references)?;
            let query = subqueries.get(*index).ok_or_else(|| {
                SQLError::Internal(format!(
                    "stored catalog routine binding cannot resolve subquery slot {index}"
                ))
            })?;
            collect_query_routine_references(query, references)?;
        }
        ScalarExpr::Star
        | ScalarExpr::QualifiedStar(_)
        | ScalarExpr::Default
        | ScalarExpr::Column(_)
        | ScalarExpr::Position(_)
        | ScalarExpr::InternalColumn(_)
        | ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::Literal(_)
        | ScalarExpr::Param(_) => {}
    }
    Ok(())
}

fn collect_many_routine_references(
    expressions: &[ScalarExpr],
    subqueries: &[QueryPlan],
    references: &mut Vec<BoundRoutineReference>,
) -> Result<(), SQLError> {
    for expression in expressions {
        collect_scalar_routine_references(expression, subqueries, references)?;
    }
    Ok(())
}

fn collect_case_routine_references(
    base: Option<&ScalarExpr>,
    when: &[(ScalarExpr, ScalarExpr)],
    else_branch: Option<&ScalarExpr>,
    subqueries: &[QueryPlan],
    references: &mut Vec<BoundRoutineReference>,
) -> Result<(), SQLError> {
    if let Some(base) = base {
        collect_scalar_routine_references(base, subqueries, references)?;
    }
    for (condition, result) in when {
        collect_scalar_routine_references(condition, subqueries, references)?;
        collect_scalar_routine_references(result, subqueries, references)?;
    }
    if let Some(branch) = else_branch {
        collect_scalar_routine_references(branch, subqueries, references)?;
    }
    Ok(())
}

fn collect_window_routine_references(
    name: &str,
    args: &[ScalarExpr],
    spec: &uqa_execution::ScalarWindowSpec,
    subqueries: &[QueryPlan],
    references: &mut Vec<BoundRoutineReference>,
) -> Result<(), SQLError> {
    for argument in args {
        collect_scalar_routine_references(argument, subqueries, references)?;
    }
    for expression in &spec.partition_by {
        collect_scalar_routine_references(expression, subqueries, references)?;
    }
    for order in &spec.order_by {
        collect_scalar_routine_references(&order.expr, subqueries, references)?;
    }
    if let Some(frame) = &spec.frame {
        for bound in [&frame.start, &frame.end] {
            if let ScalarFrameBound::Preceding(inner) | ScalarFrameBound::Following(inner) = bound {
                collect_scalar_routine_references(inner, subqueries, references)?;
            }
        }
    }
    references.push(BoundRoutineReference {
        name: name.to_string(),
        binding: None,
    });
    Ok(())
}

fn mark_query_relations_bound(query: &mut QueryPlan) {
    query.relations_bound = true;
    for cte in &mut query.ctes {
        mark_query_relations_bound(&mut cte.query);
    }
    match &mut query.root {
        RelationalPlan::QueryBlock(block) => {
            if let Some(source) = &mut block.from {
                mark_source_relations_bound(source);
            }
            for subquery in &mut block.subqueries {
                mark_query_relations_bound(subquery);
            }
        }
        RelationalPlan::SetOp {
            left,
            right,
            subqueries,
            ..
        } => {
            mark_query_relations_bound(left);
            mark_query_relations_bound(right);
            for subquery in subqueries {
                mark_query_relations_bound(subquery);
            }
        }
        RelationalPlan::Values { subqueries, .. } => {
            for subquery in subqueries {
                mark_query_relations_bound(subquery);
            }
        }
    }
}

fn mark_source_relations_bound(source: &mut SourcePlan) {
    match source {
        SourcePlan::Join { left, right, .. } => {
            mark_source_relations_bound(left);
            mark_source_relations_bound(right);
        }
        SourcePlan::Subquery { body, .. } => mark_query_relations_bound(body),
        SourcePlan::Table { .. }
        | SourcePlan::Values { .. }
        | SourcePlan::Function { .. }
        | SourcePlan::FunctionGroup { .. } => {}
    }
}
