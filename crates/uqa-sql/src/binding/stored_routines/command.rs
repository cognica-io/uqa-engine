//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Project each written mutation as a single analysis query. A private statement transfers its existing query arenas and CTEs into the projection.

use super::{
    mark_query_relations_bound, statement_target_outer_schema, statement_target_schema,
    AccessPathPlan, CommandPlan, ComputePlan, ConflictActionPlan, CtePlan, JoinExecutionStrategy,
    ProjectionPlan, QueryBlockPlan, QueryPlan, RelationalPlan, RowSchema, SQLError, ScalarExpr,
    SourcePlan, StatementSource, Value,
};
use crate::catalog::work::{ReferenceWork, Work};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

enum Source {
    Select(Option<Box<QueryPlan>>),
    Relation(Option<Box<SourcePlan>>),
    Merge {
        name: String,
        qualifier: String,
        alias: Option<String>,
        descendants: bool,
        source: Box<SourcePlan>,
    },
}

struct Parts {
    ctes: Vec<CtePlan>,
    source: Source,
    subqueries: Vec<QueryPlan>,
}

enum Statement<'a> {
    Borrowed(&'a CommandPlan),
    Private(&'a mut CommandPlan),
}

impl Statement<'_> {
    fn get(&self) -> &CommandPlan {
        match self {
            Self::Borrowed(command) => command,
            Self::Private(command) => command,
        }
    }

    fn parts(self) -> Parts {
        // Ordinary catalog callers retain the input syntax. Private compilation already owns the selected fields.
        macro_rules! field {
            (read, $field:expr) => {
                $field.clone()
            };
            (take, $field:expr) => {
                std::mem::take(&mut $field)
            };
        }
        macro_rules! merge_source {
            (read, $source:expr) => {
                $source.clone()
            };
            (take, $source:expr) => {
                Box::new(std::mem::replace(
                    $source.as_mut(),
                    crate::plan::retirement::empty_source(),
                ))
            };
        }
        macro_rules! parts {
            ($command:expr, $mode:ident) => {
                match $command {
                    CommandPlan::Insert(plan) => Parts {
                        ctes: field!($mode, plan.ctes),
                        source: Source::Select(field!($mode, plan.source)),
                        subqueries: field!($mode, plan.subqueries),
                    },
                    CommandPlan::Update(plan) => Parts {
                        ctes: field!($mode, plan.ctes),
                        source: Source::Relation(field!($mode, plan.source)),
                        subqueries: field!($mode, plan.subqueries),
                    },
                    CommandPlan::Delete(plan) => Parts {
                        ctes: field!($mode, plan.ctes),
                        source: Source::Relation(field!($mode, plan.source)),
                        subqueries: field!($mode, plan.subqueries),
                    },
                    CommandPlan::Merge(plan) => Parts {
                        ctes: field!($mode, plan.ctes),
                        subqueries: field!($mode, plan.subqueries),
                        source: Source::Merge {
                            name: field!($mode, plan.target),
                            qualifier: field!($mode, plan.target_qualifier),
                            alias: field!($mode, plan.target_alias),
                            descendants: plan.include_descendants,
                            source: merge_source!($mode, plan.source),
                        },
                    },
                    _ => unreachable!("validated mutation"),
                }
            };
        }
        match self {
            Self::Borrowed(command) => parts!(command, read),
            Self::Private(command) => parts!(command, take),
        }
    }
}

fn outer(
    context: &impl StatementSource,
    statement: &CommandPlan,
) -> Result<Option<RowSchema>, SQLError> {
    if matches!(statement, CommandPlan::Merge(_)) {
        return Ok(Some(RowSchema::default()));
    }
    if matches!(statement, CommandPlan::Notify { .. }) {
        return Ok(None);
    }
    let (Some(table), Some(qualifier), Some(aliases)) = (
        statement.mutation_target(),
        statement.target_qualifier(),
        statement.returning_aliases(),
    ) else {
        return Err(SQLError::Internal(
            "catalog-owned statement lowered to an unsupported command".into(),
        ));
    };
    let mut row = statement_target_outer_schema(context, table, qualifier, aliases)?;
    if matches!(statement, CommandPlan::Insert(plan) if matches!(plan.on_conflict.as_ref().map(|conflict| &conflict.action), Some(ConflictActionPlan::Update { .. })))
    {
        let target = statement_target_schema(context, table, qualifier)?;
        let excluded = RowSchema::with_relation_qualifier(&target, "excluded");
        row = RowSchema::join(&row, &excluded, std::iter::empty::<String>());
    }
    Ok(Some(row))
}

pub(super) fn command_statement_query(
    context: &impl StatementSource,
    command: &CommandPlan,
) -> Result<Option<(QueryPlan, Option<RowSchema>)>, SQLError> {
    let control = ProductionControl::uncontrolled();
    Ok(crate::catalog::work::inline(project(
        context,
        Statement::Borrowed(command),
        &control,
        &mut (),
    ))?
    .map(|(query, row)| {
        (
            query.into_uncontrolled().expect("ordinary command syntax"),
            Some(row),
        )
    }))
}

pub(super) async fn project_from(
    context: &impl StatementSource,
    command: &mut CommandPlan,
    control: &ProductionControl<'_>,
    work: &mut (impl Work + Send),
) -> Result<Option<(Produced<QueryPlan>, RowSchema)>, SQLError> {
    project(context, Statement::Private(command), control, work).await
}

async fn project<W: Work + Send>(
    context: &impl StatementSource,
    statement: Statement<'_>,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<Option<(Produced<QueryPlan>, RowSchema)>, SQLError> {
    control.check()?;
    let Some(row) = outer(context, statement.get())? else {
        return Ok(None);
    };
    let mut projections = ProductionVec::new(*control);
    for field in statement.get().syntax_expression_steps() {
        work.checkpoint().await?;
        control.check()?;
        let Some(value) = field else {
            continue;
        };
        let (expr, memory) = if matches!(value, ScalarExpr::Default) {
            (
                ScalarExpr::Literal(Value::Int(1)),
                control.empty_reservation(),
            )
        } else if W::MAY_SUSPEND {
            value
                .clone_with_work(control, &mut ReferenceWork { control, work })
                .await?
                .into_parts()
        } else {
            (value.clone(), control.empty_reservation())
        };
        projections.push_produced(control.finish(ProjectionPlan { expr, alias: None }, memory)?)?;
    }
    projections.push_produced(control.finish(
        ProjectionPlan {
            expr: ScalarExpr::Literal(Value::Int(1)),
            alias: None,
        },
        control.empty_reservation(),
    )?)?;
    let (projections, mut memory) = projections.finish()?.into_parts();
    memory = control.combine(memory, control.reserve(size_of::<QueryBlockPlan>())?);
    if matches!(statement.get(), CommandPlan::Merge(_)) {
        memory = control.combine(memory, control.reserve(2 * size_of::<SourcePlan>())?);
    }
    let Parts {
        ctes,
        source,
        subqueries,
    } = statement.parts();
    let source = match source {
        Source::Relation(source) => source.map(|source| *source),
        Source::Select(source) => match source {
            None => None,
            Some(body) => {
                let (alias, receipt) = control
                    .copy_text("__uqa_catalog_statement_source")?
                    .into_parts();
                memory = control.combine(memory, receipt);
                Some(SourcePlan::Subquery {
                    captured_output: None,
                    lateral: false,
                    body,
                    alias: Some(alias),
                    column_aliases: Vec::new(),
                })
            }
        },
        Source::Merge {
            name,
            qualifier,
            alias,
            descendants,
            source,
        } => {
            let target = SourcePlan::Table {
                executable_query: None,
                bound_columns: None,
                name,
                qualifier,
                alias,
                column_aliases: Vec::new(),
                include_descendants: descendants,
            };
            Some(SourcePlan::Join {
                comma: false,
                left: Box::new(target),
                right: source,
                kind: crate::ast::JoinKind::Cross,
                on: None,
                using: None,
                natural: false,
                alias: None,
                column_aliases: Vec::new(),
                lateral: false,
                strategy: JoinExecutionStrategy::default(),
            })
        }
    };
    let mut query = QueryPlan {
        analyzed_output: None,
        analyzed_source: None,
        analyzed_grouping: None,
        relation_privilege_subject: None,
        relations_bound: true,
        ctes,
        root: RelationalPlan::QueryBlock(Box::new(QueryBlockPlan {
            deferred_outer_rewrites: Vec::new(),
            privilege_columns: uqa_core::memory::OwnedSet::new(),
            windows: Vec::new(),
            projections,
            from: source,
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
            subqueries,
            access: AccessPathPlan::Row,
            locking: Vec::new(),
        })),
    };
    mark_query_relations_bound(&mut query);
    Ok(Some((control.finish(query, memory)?, row)))
}
