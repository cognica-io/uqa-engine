//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact routine binding for catalog-owned statements.

use crate::ast::FunctionBinding;
use crate::plan::{
    AccessPathPlan, CommandPlan, ComputePlan, ConflictActionPlan, CtePlan, DeletePlan, InsertPlan,
    JoinExecutionStrategy, MergePlan, ProjectionPlan, QueryBlockPlan, QueryPlan, RelationalPlan,
    SourcePlan, UnifiedPlan, UpdatePlan,
};
use crate::SQLError;
use crate::{RowSchema, ScalarExpr};

use crate::{binding::context::BindingContext, routines::RoutineResolution};
use uqa_core::Value;

/// Routine metadata and immutable namespace inputs for a stored statement.
pub struct CatalogRoutineContext<'a, 'q> {
    pub routines: &'a dyn RoutineResolution,
    pub binding: &'a BindingContext<'q>,
}

pub struct BoundStatementRoutines {
    pub query: Option<QueryPlan>,
    /// What binding recorded for the statement's syntax, in syntax order.
    pub sites: super::syntax_sites::SyntaxSites,
    /// The columns a query statement returns; a command's binding query is not its result.
    pub query_output: Option<RowSchema>,
}

#[derive(Debug, Clone)]
pub struct BoundRoutineReference {
    pub name: String,
    pub binding: Option<FunctionBinding>,
}

struct CommandRoutineInputs {
    ctes: Vec<CtePlan>,
    source: Option<SourcePlan>,
    expressions: Vec<ScalarExpr>,
    subqueries: Vec<QueryPlan>,
    outer: RowSchema,
}

/// Bind a copy of a catalog-owned statement lowered from its stored syntax; `params` types the positional parameters its syntax references.
pub fn bind_catalog_statement_routines(
    context: &CatalogRoutineContext<'_, '_>,
    plan: &UnifiedPlan,
    params: &[crate::SQLParam],
) -> Result<BoundStatementRoutines, SQLError> {
    let lowered = match plan {
        UnifiedPlan::Query(query) => {
            let mut query = (**query).clone();
            mark_query_relations_bound(&mut query);
            Some((query, None))
        }
        UnifiedPlan::Command(command) => command_statement_query(context, command)?,
    };
    let Some((lowered, outer)) = lowered else {
        return Ok(BoundStatementRoutines {
            query: None,
            sites: super::syntax_sites::SyntaxSites::default(),
            query_output: None,
        });
    };
    let mut query = lowered.clone();
    let output = crate::binding::bind_syntax_query_plan_routines(
        context.routines,
        &mut query,
        params,
        context.binding,
        outer.as_ref(),
    )?;
    let sites = super::syntax_sites::query_syntax_sites(&lowered, &query)?;
    Ok(BoundStatementRoutines {
        query: Some(query),
        sites,
        query_output: matches!(plan, UnifiedPlan::Query(_)).then_some(output),
    })
}

pub fn mark_catalog_statement_relations_bound(plan: &mut UnifiedPlan) -> Result<(), SQLError> {
    match plan {
        UnifiedPlan::Query(query) => mark_query_relations_bound(query),
        UnifiedPlan::Command(command) => match command.as_mut() {
            CommandPlan::Insert(plan) => {
                plan.relations_bound = true;
                for cte in &mut plan.ctes {
                    mark_cte_relations_bound(&mut cte.body);
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
                    mark_cte_relations_bound(&mut cte.body);
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
                    mark_cte_relations_bound(&mut cte.body);
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
                for cte in &mut plan.ctes {
                    mark_cte_relations_bound(&mut cte.body);
                }
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

/// A command's syntax as one query whose select list holds the command's expressions in syntax order, so the command binds and reads back like a query. `DEFAULT` markers have no scalar syntax to bind.
fn command_statement_query(
    context: &CatalogRoutineContext<'_, '_>,
    command: &CommandPlan,
) -> Result<Option<(QueryPlan, Option<RowSchema>)>, SQLError> {
    let Some(inputs) = command_statement_routine_inputs(context, command)? else {
        return Ok(None);
    };
    let projections = inputs
        .expressions
        .into_iter()
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
    Ok(Some((query, Some(inputs.outer))))
}

fn command_statement_routine_inputs(
    context: &CatalogRoutineContext<'_, '_>,
    command: &CommandPlan,
) -> Result<Option<CommandRoutineInputs>, SQLError> {
    match command {
        CommandPlan::Insert(plan) => insert_statement_routine_inputs(context, plan).map(Some),
        CommandPlan::Update(plan) => update_statement_routine_inputs(context, plan).map(Some),
        CommandPlan::Delete(plan) => delete_statement_routine_inputs(context, plan).map(Some),
        CommandPlan::Merge(plan) => Ok(Some(merge_statement_routine_inputs(plan))),
        CommandPlan::Notify { .. } => Ok(None),
        _ => Err(SQLError::Internal(
            "catalog-owned statement lowered to an unsupported command".into(),
        )),
    }
}

fn merge_statement_routine_inputs(plan: &MergePlan) -> CommandRoutineInputs {
    let target = SourcePlan::Table {
        bound_columns: None,
        name: plan.target.clone(),
        qualifier: plan.target_qualifier.clone(),
        alias: plan.target_alias.clone(),
        column_aliases: Vec::new(),
        include_descendants: plan.include_descendants,
    };
    let source = SourcePlan::Join {
        left: Box::new(target),
        right: plan.source.clone(),
        kind: crate::ast::JoinKind::Cross,
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
            crate::plan::MergeWhenPlan::UpdateMatched {
                condition,
                assignments,
            }
            | crate::plan::MergeWhenPlan::UpdateNotMatchedBySource {
                condition,
                assignments,
            } => {
                expressions.extend(condition.iter().cloned());
                expressions.extend(
                    assignments
                        .iter()
                        .flat_map(crate::plan::AssignmentPlan::expressions)
                        .cloned(),
                );
            }
            crate::plan::MergeWhenPlan::InsertNotMatched {
                condition,
                columns,
                values,
                ..
            } => {
                expressions.extend(condition.iter().cloned());
                expressions.extend(
                    columns
                        .iter()
                        .flat_map(crate::ast::AssignmentTarget::expressions)
                        .cloned(),
                );
                expressions.extend(values.iter().cloned());
            }
            crate::plan::MergeWhenPlan::DeleteMatched { condition }
            | crate::plan::MergeWhenPlan::DeleteNotMatchedBySource { condition }
            | crate::plan::MergeWhenPlan::NothingMatched { condition }
            | crate::plan::MergeWhenPlan::NothingNotMatched { condition }
            | crate::plan::MergeWhenPlan::NothingNotMatchedBySource { condition } => {
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
        ctes: plan.ctes.clone(),
        source: Some(source),
        expressions,
        subqueries: plan.subqueries.clone(),
        outer: RowSchema::default(),
    }
}

fn insert_statement_routine_inputs(
    context: &CatalogRoutineContext<'_, '_>,
    plan: &InsertPlan,
) -> Result<CommandRoutineInputs, SQLError> {
    let mut expressions = plan
        .columns
        .iter()
        .flat_map(crate::ast::AssignmentTarget::expressions)
        .chain(plan.rows.iter().flatten())
        .cloned()
        .collect::<Vec<_>>();
    if let Some(conflict) = &plan.on_conflict {
        expressions.extend(conflict.expressions.iter().cloned());
        expressions.extend(conflict.predicate.iter().map(Box::as_ref).cloned());
        if let ConflictActionPlan::Update {
            assignments,
            predicate,
        } = &conflict.action
        {
            expressions.extend(
                assignments
                    .iter()
                    .flat_map(crate::plan::AssignmentPlan::expressions)
                    .cloned(),
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
    let mut outer = statement_target_outer_schema(
        context,
        &plan.table,
        &plan.target_qualifier,
        &plan.returning_aliases,
    )?;
    // `ON CONFLICT DO UPDATE` also sees the proposed row as `excluded`.
    if matches!(
        plan.on_conflict.as_ref().map(|conflict| &conflict.action),
        Some(ConflictActionPlan::Update { .. })
    ) {
        let target = statement_target_schema(context, &plan.table, &plan.target_qualifier)?;
        let excluded = RowSchema::with_qualified_types(
            "excluded",
            target.columns().to_vec(),
            target.column_types().to_vec(),
        );
        outer = RowSchema::join(&outer, &excluded, std::iter::empty::<String>());
    }
    Ok(CommandRoutineInputs {
        ctes: plan.ctes.clone(),
        source,
        expressions,
        subqueries: plan.subqueries.clone(),
        outer,
    })
}

fn update_statement_routine_inputs(
    context: &CatalogRoutineContext<'_, '_>,
    plan: &UpdatePlan,
) -> Result<CommandRoutineInputs, SQLError> {
    let mut expressions = plan
        .assignments
        .iter()
        .flat_map(crate::plan::AssignmentPlan::expressions)
        .cloned()
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
            context,
            &plan.table,
            &plan.target_qualifier,
            &plan.returning_aliases,
        )?,
    })
}

fn delete_statement_routine_inputs(
    context: &CatalogRoutineContext<'_, '_>,
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
            context,
            &plan.table,
            &plan.target_qualifier,
            &plan.returning_aliases,
        )?,
    })
}

fn statement_target_outer_schema(
    context: &CatalogRoutineContext<'_, '_>,
    table: &str,
    target_qualifier: &str,
    aliases: &crate::ast::ReturningAliases,
) -> Result<RowSchema, SQLError> {
    let target = statement_target_schema(context, table, target_qualifier)?;
    Ok(crate::semantics::returning_expression_schema(
        &target,
        target_qualifier,
        aliases,
        None,
    ))
}

fn statement_target_schema(
    context: &CatalogRoutineContext<'_, '_>,
    table: &str,
    target_qualifier: &str,
) -> Result<RowSchema, SQLError> {
    let target = crate::binding::analyze_source_plan_schema(
        context.routines,
        &SourcePlan::Table {
            bound_columns: None,
            name: table.to_string(),
            qualifier: target_qualifier.to_string(),
            alias: None,
            column_aliases: Vec::new(),
            include_descendants: true,
        },
        &[],
        context.binding,
        None,
    )?;
    Ok(RowSchema::with_types(
        target.columns().to_vec(),
        target.column_types().to_vec(),
    ))
}

/// Routine identities of a bound stored expression in syntax order.
pub fn collect_expression_routine_references(
    expression: &crate::plan::ExpressionPlan,
) -> Result<Vec<BoundRoutineReference>, SQLError> {
    Ok(super::syntax_sites::expression_syntax_sites(expression, expression)?.routines)
}

fn mark_cte_relations_bound(body: &mut crate::plan::CtePlanBody) {
    match body {
        crate::plan::CtePlanBody::Query(query) => mark_query_relations_bound(query),
        crate::plan::CtePlanBody::Command(command) => {
            match command.as_mut() {
                CommandPlan::Insert(plan) => {
                    plan.relations_bound = true;
                    plan.target_relation_bound = true;
                }
                CommandPlan::Update(plan) => {
                    plan.relations_bound = true;
                    plan.target_relation_bound = true;
                }
                CommandPlan::Delete(plan) => {
                    plan.relations_bound = true;
                    plan.target_relation_bound = true;
                }
                _ => {}
            }
            if let Some(ctes) = command.ctes_mut() {
                for cte in ctes {
                    mark_cte_relations_bound(&mut cte.body);
                }
            }
            for query in command.query_inputs_mut() {
                mark_query_relations_bound(query);
            }
            if let Some(source) = command.source_input_mut() {
                mark_source_relations_bound(source);
            }
        }
    }
}

fn mark_query_relations_bound(query: &mut QueryPlan) {
    query.relations_bound = true;
    for cte in &mut query.ctes {
        mark_cte_relations_bound(&mut cte.body);
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

pub mod analysis;
