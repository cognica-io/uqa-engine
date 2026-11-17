//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered metadata work borrows disjoint original slots instead of moving syntax or recursing through the call stack.

use super::{
    install_aliases, install_source_updates, MetadataObserver, MutableExecutablePlan, QueryPlan,
    RewriteMemory, SQLError, SourcePlan, Walker,
};
use crate::{
    catalog::work::{inline, Work},
    construction::Stack,
    plan::{
        retirement::construction::{Context, Prefix},
        CommandPlan, CtePlan, CtePlanBody, ExpressionPlan, ProjectionPlan, RelationalPlan,
        UnifiedPlan,
    },
};
use std::slice::IterMut;
use uqa_core::memory::ProductionControl;

mod commands;

type Memory<'a> = Option<&'a mut RewriteMemory>;
type Result<T> = std::result::Result<T, SQLError>;

fn field<'a>(memory: Memory<'a>, name: &str) -> Memory<'a> {
    memory.map(|memory| memory.field_mut(name).as_mut().expect("metadata field"))
}

fn fields<'a, const N: usize>(
    memory: Memory<'a>,
    names: [&str; N],
) -> [Option<&'a mut Option<RewriteMemory>>; N] {
    memory.map_or_else(
        || std::array::from_fn(|_| None),
        |memory| memory.fields_mut(names).map(Some),
    )
}

fn present(memory: Option<&mut Option<RewriteMemory>>) -> Memory<'_> {
    memory.map(|memory| memory.as_mut().expect("present metadata field"))
}

struct Elements<'a, T> {
    values: IterMut<'a, T>,
    memory: Option<IterMut<'a, super::super::memory::RewriteField>>,
}

impl<'a, T> Elements<'a, T> {
    fn new(values: &'a mut [T], memory: Memory<'a>) -> Self {
        let count = values.len();
        let memory = memory.map(|memory| {
            let RewriteMemory::Fields { fields, .. } = memory else {
                panic!("metadata vector owns plan fields")
            };
            assert_eq!(count, fields.len(), "metadata vector matches its payload");
            fields.iter_mut()
        });
        Self {
            values: values.iter_mut(),
            memory,
        }
    }

    fn next(&mut self) -> Option<(&'a mut T, Memory<'a>)> {
        let value = self.values.next()?;
        let memory = self.memory.as_mut().map(|memory| {
            memory
                .next()
                .expect("metadata vector item")
                .memory
                .as_mut()
                .expect("owned metadata item")
        });
        Some((value, memory))
    }
}

enum Node<'a> {
    Executable(&'a mut UnifiedPlan, Memory<'a>),
    Query(&'a mut QueryPlan, Memory<'a>),
    Root(&'a mut RelationalPlan, Memory<'a>),
    Source(&'a mut SourcePlan, Memory<'a>),
    Command(&'a mut CommandPlan, Memory<'a>),
    Projections(&'a mut [ProjectionPlan], Memory<'a>),
    Ctes(Elements<'a, CtePlan>),
    Queries(Elements<'a, QueryPlan>),
    Expressions(Elements<'a, ExpressionPlan>),
}

struct Task<'a> {
    node: Node<'a>,
    cte_command: bool,
}

struct Tasks<'a> {
    pending: Stack<(Task<'a>, usize), 4>,
    depth: usize,
}
impl<'a> Tasks<'a> {
    fn new(control: ProductionControl<'_>) -> Self {
        Self {
            pending: Stack::new(control),
            depth: 0,
        }
    }
    fn pop(&mut self) -> Option<Task<'a>> {
        let (task, depth) = self.pending.pop()?;
        self.depth = depth;
        Some(task)
    }
}

fn push<'a>(tasks: &mut Tasks<'a>, node: Node<'a>, cte_command: bool) -> Result<()> {
    let depth = tasks
        .depth
        .checked_add(1)
        .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?;
    Ok(tasks.pending.push((Task { node, cte_command }, depth))?)
}

fn resume<'a>(tasks: &mut Tasks<'a>, node: Node<'a>, cte_command: bool) -> Result<()> {
    Ok(tasks
        .pending
        .push((Task { node, cte_command }, tasks.depth))?)
}

impl Walker<'_, '_> {
    pub(super) fn plan(&mut self, plan: &mut MutableExecutablePlan) -> Result<()> {
        let control = self.control;
        inline(plan.apply_metadata_from(self, &control, &mut ()))
    }

    pub(super) fn executable(&mut self, plan: &mut UnifiedPlan, memory: Memory<'_>) -> Result<()> {
        let control = self.control;
        inline(walk(
            Node::Executable(plan, memory),
            None,
            self,
            &control,
            &mut (),
        ))
    }
}

pub(crate) trait MetadataTarget {
    fn apply_metadata_from<W: Work + Send, O: MetadataObserver<W>>(
        &mut self,
        observe: &mut O,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> impl std::future::Future<Output = Result<()>>;
}

impl MutableExecutablePlan {
    fn metadata_root(&mut self, control: &ProductionControl<'_>) -> Node<'_> {
        match self {
            Self::Query(query) => {
                query.assert_owner(control);
                Node::Query(&mut query.query, query.memory.as_mut())
            }
            Self::Command(command) => {
                command.assert_owner(control);
                Node::Command(&mut command.command, command.memory.as_mut())
            }
        }
    }
}

impl MetadataTarget for MutableExecutablePlan {
    fn apply_metadata_from<W: Work + Send, O: MetadataObserver<W>>(
        &mut self,
        observe: &mut O,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> impl std::future::Future<Output = Result<()>> {
        walk(self.metadata_root(control), None, observe, control, work)
    }
}

impl MetadataTarget for Prefix<MutableExecutablePlan> {
    fn apply_metadata_from<W: Work + Send, O: MetadataObserver<W>>(
        &mut self,
        observe: &mut O,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> impl std::future::Future<Output = Result<()>> {
        let context = self.context().clone();
        walk(
            self.metadata_root(control),
            Some(context),
            observe,
            control,
            work,
        )
    }
}

fn push_query<'a>(
    tasks: &mut Tasks<'a>,
    (query, memory): (&'a mut QueryPlan, Memory<'a>),
    cte_command: bool,
) -> Result<()> {
    let [ctes, root] = fields(memory, ["ctes", "root"]);
    push(
        tasks,
        Node::Root(&mut query.root, present(root)),
        cte_command,
    )?;
    push(
        tasks,
        Node::Ctes(Elements::new(&mut query.ctes, present(ctes))),
        cte_command,
    )?;
    Ok(())
}

async fn walk<W: Work + Send, O: MetadataObserver<W>>(
    root: Node<'_>,
    context: Option<Context>,
    observe: &mut O,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<()> {
    let mut tasks = Tasks::new(*control);
    push(&mut tasks, root, false)?;
    while let Some(Task { node, cte_command }) = tasks.pop() {
        control.check()?;
        work.checkpoint().await?;
        control.check()?;
        match node {
            Node::Executable(plan, memory) => {
                let child = match plan {
                    UnifiedPlan::Query(query) => Node::Query(query, field(memory, "query")),
                    UnifiedPlan::Command(command) => {
                        Node::Command(command, field(memory, "command"))
                    }
                };
                push(&mut tasks, child, cte_command)?;
            }
            Node::Query(query, mut memory) => {
                if observe.mark_relations() {
                    query.relations_bound = true;
                }
                if let Some(update) = observe.query(query, control, work).await? {
                    update
                        .install_from(
                            query,
                            memory.as_deref_mut(),
                            super::replacement::Scope {
                                context: context.as_ref(),
                                depth: tasks.depth,
                            },
                            control,
                            work,
                        )
                        .await?;
                }
                push_query(&mut tasks, (query, memory), cte_command)?;
            }
            Node::Root(root, memory) => push_root(&mut tasks, root, memory, cte_command)?,
            Node::Source(source, mut memory) => {
                if let Some(replacement) = observe.source_replacement(source, control, work).await?
                {
                    if let Some(mut discarded) = replacement.install(
                        source,
                        memory.as_deref_mut(),
                        super::replacement::Scope {
                            context: context.as_ref(),
                            depth: tasks.depth,
                        },
                        control,
                    )? {
                        discarded.retire_from(control, work).await?;
                    }
                }
                if let Some(updates) = observe.source(source, control, work).await? {
                    install_source_updates(source, memory.as_deref_mut(), updates, control, work)
                        .await?;
                }
                push_source(
                    &mut tasks,
                    source,
                    memory,
                    cte_command,
                    observe.visit_table_queries(),
                )?;
            }
            Node::Command(command, memory) => {
                commands::push_command(
                    &mut tasks,
                    command,
                    memory,
                    cte_command,
                    observe.mark_relations(),
                )?;
            }
            Node::Projections(projections, memory) => {
                if let Some(aliases) = observe.projections(projections, control, work).await? {
                    install_aliases(projections, memory, aliases, control, work).await?;
                }
            }
            Node::Ctes(items) => push_cte(&mut tasks, items, cte_command)?,
            Node::Queries(items) => push_queries(&mut tasks, items, cte_command)?,
            Node::Expressions(mut items) => {
                if let Some((expression, memory)) = items.next() {
                    resume(&mut tasks, Node::Expressions(items), cte_command)?;
                    push(
                        &mut tasks,
                        Node::Queries(Elements::new(
                            &mut expression.subqueries,
                            field(memory, "subqueries"),
                        )),
                        cte_command,
                    )?;
                }
            }
        }
    }
    control.check().map_err(Into::into)
}

fn push_queries<'a>(
    tasks: &mut Tasks<'a>,
    mut items: Elements<'a, QueryPlan>,
    cte_command: bool,
) -> Result<()> {
    if let Some((query, memory)) = items.next() {
        resume(tasks, Node::Queries(items), cte_command)?;
        push(
            tasks,
            Node::Query(query, field(memory, "query")),
            cte_command,
        )?;
    }
    Ok(())
}

fn push_cte<'a>(
    tasks: &mut Tasks<'a>,
    mut items: Elements<'a, CtePlan>,
    cte_command: bool,
) -> Result<()> {
    if let Some((cte, memory)) = items.next() {
        resume(tasks, Node::Ctes(items), cte_command)?;
        let memory = field(memory, "body");
        let (body, nested_command) = match &mut cte.body {
            CtePlanBody::Query(query) => (Node::Query(query, field(memory, "query")), cte_command),
            CtePlanBody::Command(command) => {
                (Node::Command(command, field(memory, "command")), true)
            }
        };
        push(tasks, body, nested_command)?;
    }
    Ok(())
}

fn push_root<'a>(
    tasks: &mut Tasks<'a>,
    root: &'a mut RelationalPlan,
    memory: Memory<'a>,
    cte_command: bool,
) -> Result<()> {
    match root {
        RelationalPlan::QueryBlock(block) => {
            let memory = field(memory, "block");
            let [projections, source, subqueries] =
                fields(memory, ["projections", "from", "subqueries"]);
            push(
                tasks,
                Node::Queries(Elements::new(&mut block.subqueries, present(subqueries))),
                cte_command,
            )?;
            if let Some(value) = &mut block.from {
                push(
                    tasks,
                    Node::Source(value, field(present(source), "source")),
                    cte_command,
                )?;
            }
            push(
                tasks,
                Node::Projections(&mut block.projections, present(projections)),
                cte_command,
            )?;
        }
        RelationalPlan::SetOp {
            left,
            right,
            subqueries,
            ..
        } => {
            let [left_memory, right_memory, queries] =
                fields(memory, ["left", "right", "subqueries"]);
            push(
                tasks,
                Node::Queries(Elements::new(subqueries, present(queries))),
                cte_command,
            )?;
            push(
                tasks,
                Node::Query(right, field(present(right_memory), "query")),
                cte_command,
            )?;
            push(
                tasks,
                Node::Query(left, field(present(left_memory), "query")),
                cte_command,
            )?;
        }
        RelationalPlan::Values { subqueries, .. } => {
            push(
                tasks,
                Node::Queries(Elements::new(subqueries, field(memory, "subqueries"))),
                cte_command,
            )?;
        }
    }
    Ok(())
}

fn push_source<'a>(
    tasks: &mut Tasks<'a>,
    source: &'a mut SourcePlan,
    memory: Memory<'a>,
    cte_command: bool,
    visit_table_queries: bool,
) -> Result<()> {
    match source {
        SourcePlan::Join { left, right, .. } => {
            let [left_memory, right_memory] = fields(memory, ["left", "right"]);
            push(
                tasks,
                Node::Source(right, field(present(right_memory), "source")),
                cte_command,
            )?;
            push(
                tasks,
                Node::Source(left, field(present(left_memory), "source")),
                cte_command,
            )?;
        }
        SourcePlan::Subquery { body, .. } => {
            push(
                tasks,
                Node::Query(body, field(field(memory, "body"), "query")),
                cte_command,
            )?;
        }
        SourcePlan::Table {
            executable_query: Some(body),
            ..
        } if visit_table_queries => {
            push(
                tasks,
                Node::Query(body, field(field(memory, "executable_query"), "query")),
                cte_command,
            )?;
        }
        _ => (),
    }
    Ok(())
}
