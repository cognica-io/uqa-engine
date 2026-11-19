//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Command metadata preserves RETURNING, CTE, query-arena and source order in the shared borrowed work list.

use super::{field, fields, present, push, Elements, Memory, Node, Result, Tasks};
use crate::plan::{CommandPlan, CtePlan, ProjectionPlan, QueryPlan, SourcePlan};

enum Input<'a> {
    Query(&'a mut QueryPlan),
    Source(&'a mut SourcePlan),
}

pub(super) fn push_command<'a>(
    tasks: &mut Tasks<'a>,
    command: &'a mut CommandPlan,
    memory: Memory<'a>,
    cte_command: bool,
    mark_relations: bool,
) -> Result<()> {
    if mark_relations {
        if cte_command {
            command.mark_stored_cte_target_bound();
        } else {
            match command {
                CommandPlan::Insert(plan) => plan.relations_bound = true,
                CommandPlan::Update(plan) => plan.relations_bound = true,
                CommandPlan::Delete(plan) => plan.relations_bound = true,
                _ => (),
            }
        }
    }
    match command {
        CommandPlan::CreateView { query, .. }
        | CommandPlan::CreateTableAs { query, .. }
        | CommandPlan::CreateMaterializedView { query, .. }
        | CommandPlan::DeclareCursor { query, .. } => {
            push(
                tasks,
                Node::Query(query, field(field(memory, "query"), "query")),
                cte_command,
            )?;
        }
        CommandPlan::Explain { body, .. } | CommandPlan::Prepare { body, .. } => {
            push(
                tasks,
                Node::Executable(body, field(memory, "body")),
                cte_command,
            )?;
        }
        CommandPlan::Call { args, .. } => {
            push(
                tasks,
                Node::Expressions(Elements::new(args, field(memory, "args"))),
                cte_command,
            )?;
        }
        CommandPlan::Execute { params, .. } => {
            push(
                tasks,
                Node::Expressions(Elements::new(params, field(memory, "params"))),
                cte_command,
            )?;
        }
        CommandPlan::Insert(plan) => mutation(
            tasks,
            &mut plan.returning,
            &mut plan.ctes,
            &mut plan.subqueries,
            plan.source.as_deref_mut().map(Input::Query),
            field(memory, "value0"),
            cte_command,
        )?,
        CommandPlan::Update(plan) => mutation(
            tasks,
            &mut plan.returning,
            &mut plan.ctes,
            &mut plan.subqueries,
            plan.source.as_deref_mut().map(Input::Source),
            field(memory, "value0"),
            cte_command,
        )?,
        CommandPlan::Delete(plan) => mutation(
            tasks,
            &mut plan.returning,
            &mut plan.ctes,
            &mut plan.subqueries,
            plan.source.as_deref_mut().map(Input::Source),
            field(memory, "value0"),
            cte_command,
        )?,
        CommandPlan::Merge(plan) => mutation(
            tasks,
            &mut plan.returning,
            &mut plan.ctes,
            &mut plan.subqueries,
            Some(Input::Source(&mut plan.source)),
            field(memory, "value0"),
            cte_command,
        )?,
        _ => (),
    }
    Ok(())
}

fn mutation<'a>(
    tasks: &mut Tasks<'a>,
    returning: &'a mut [ProjectionPlan],
    ctes: &'a mut [CtePlan],
    subqueries: &'a mut [QueryPlan],
    input: Option<Input<'a>>,
    memory: Memory<'a>,
    cte_command: bool,
) -> Result<()> {
    let [projections, cte_memory, source, queries] =
        fields(memory, ["returning", "ctes", "source", "subqueries"]);
    let arena = Node::Queries(Elements::new(subqueries, present(queries)));
    match input {
        Some(Input::Query(query)) => {
            push(tasks, arena, cte_command)?;
            push(
                tasks,
                Node::Query(query, field(present(source), "query")),
                cte_command,
            )?;
        }
        Some(Input::Source(value)) => {
            push(
                tasks,
                Node::Source(value, field(present(source), "source")),
                cte_command,
            )?;
            push(tasks, arena, cte_command)?;
        }
        None => push(tasks, arena, cte_command)?,
    }
    push(
        tasks,
        Node::Ctes(Elements::new(ctes, present(cte_memory))),
        cte_command,
    )?;
    push(
        tasks,
        Node::Projections(returning, present(projections)),
        cte_command,
    )?;
    Ok(())
}
