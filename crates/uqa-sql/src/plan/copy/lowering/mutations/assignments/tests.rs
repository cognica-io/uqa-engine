//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::super::input::{ScalarInput, TextInput};
use super::{conflict, input as fields, merge, targets, Copying, Result};
use crate::{
    ast::{
        AssignmentTarget, AssignmentTargets, Expr, MergeWhen, OnConflict, SelectStmt, Statement,
    },
    construction::{self, Work},
    plan::{
        mutable::memory::RewriteMemory,
        retirement::{Pending, Retire},
        AssignmentPlan, CommandPlan, ConflictPlan, ExpressionPlan, MergeWhenPlan, OwnedScalarExpr,
        RelationalPlan, UnifiedPlan,
    },
    SQLError, ScalarExpr,
};
use std::sync::Arc;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::{BudgetedVec, MemoryBudget, Produced, ProductionControl},
    CancellationToken, Value,
};

mod retirement;
mod semantics;

#[derive(Clone)]
enum Source {
    Targets(Vec<AssignmentTarget>),
    Assignments(Vec<(AssignmentTargets, Expr)>),
    Conflict(OnConflict),
    Merge(Vec<MergeWhen>),
}

#[derive(Debug, serde::Serialize)]
enum Field {
    Targets(Vec<AssignmentTarget<ScalarExpr>>),
    Assignments(Vec<AssignmentPlan>),
    Conflict(ConflictPlan),
    Merge(Vec<MergeWhenPlan>),
}
impl Retire for Field {
    fn retire_step(
        &mut self,
        positions: &mut [usize],
        scalar: &mut Option<crate::ir::ownership::Retirement>,
    ) -> bool {
        match self {
            Self::Targets(values) => values.retire_step(positions, scalar),
            Self::Assignments(values) => values.retire_step(positions, scalar),
            Self::Conflict(value) => value.retire_step(positions, scalar),
            Self::Merge(values) => values.retire_step(positions, scalar),
        }
    }
}

struct Queries<'a> {
    work: Work<'a>,
    values: BudgetedVec<i64>,
}

impl<'s> crate::plan::scalar::ScalarQueryInput<&'s Expr, &'s crate::ast::SelectStmt>
    for Queries<'_>
{
    async fn append(
        &mut self,
        query: &'s SelectStmt,
        control: &ProductionControl<'_>,
    ) -> Result<usize> {
        self.work.checkpoint().await;
        control.check()?;
        let Expr::Literal(Value::Int(value)) = query.projections[0].expr else {
            panic!("literal query fixture");
        };
        if value == 99 {
            return Err(SQLError::Internal("rejected mutation query".into()));
        }
        let index = self.values.len();
        self.values.push(value)?;
        Ok(index)
    }
}

struct Native<'a> {
    work: Work<'a>,
    queries: Queries<'a>,
}

impl<'s> ScalarInput<&'s Expr> for Native<'_> {
    async fn checkpoint(&mut self) -> Result<()> {
        self.work.checkpoint().await;
        Ok(())
    }

    async fn scalar(
        &mut self,
        expression: &'s Expr,
        control: ProductionControl<'_>,
    ) -> Result<OwnedScalarExpr> {
        ExpressionPlan::lower_scalar_with_query_input(
            expression,
            &mut self.queries,
            &control,
            &mut self.work,
        )?
        .await
    }
}

impl<'s> TextInput<&'s str> for Native<'_> {
    async fn text(
        &mut self,
        source: &'s str,
        control: ProductionControl<'_>,
    ) -> Result<Produced<String>> {
        crate::catalog::work::copy_text(&mut self.work, source, &control).await
    }
}

fn retained_field<'s: 'a, 'c: 'a, 'a, I: ScalarInput<&'s Expr> + TextInput<&'s str> + Send + 'a>(
    source: &'s Source,
    copying: &'a mut Copying<'c>,
    input: &'a mut I,
) -> Result<construction::Read<'a, Result<Pending<Field>>>> {
    Ok(construction::Read::new(copying.control, async move {
        Ok(Pending::new(match source {
            Source::Targets(values) => Field::Targets(
                targets::construct_targets(values, copying, input)
                    .await?
                    .take(),
            ),
            Source::Assignments(values) => Field::Assignments(
                fields::construct_assignments(values, copying, input)
                    .await?
                    .take(),
            ),
            Source::Conflict(value) => Field::Conflict(
                conflict::construct_conflict(value, copying, input)
                    .await?
                    .take(),
            ),
            Source::Merge(values) => Field::Merge(
                merge::construct_merge_whens(values, copying, input)
                    .await?
                    .take(),
            ),
        }))
    })?)
}

type Output = (Pending<Field>, RewriteMemory, BudgetedVec<i64>);
type Input = ImageDecoder<Output, SQLError>;

fn input(
    source: Arc<Source>,
    native: &MemoryBudget,
    work: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> Input {
    let native = native.clone();
    let original = original.clone();
    ImageDecoder::new(work, invoking, |mut reader| async move {
        let mut channel = reader.input();
        let control = ProductionControl::new(&native, &original, channel.cancellation);
        let mut driver = construction::Driver::new(control)?;
        let source = source.as_ref();
        let native = &native;
        let output = driver.work().submit(move |work| async move {
            let mut input = Native {
                queries: Queries {
                    work: work.clone(),
                    values: BudgetedVec::new(native),
                },
                work,
            };
            let mut copying = Copying::for_rewrite(control).unwrap();
            let field = retained_field(source, &mut copying, &mut input)?.await?;
            Ok::<_, SQLError>((field, copying.into_rewrite_memory(), input.queries.values))
        })?;
        while driver.advance().await? {
            channel.checkpoint().await?;
        }
        construction::receive(output).await
    })
    .unwrap()
}

fn advance(input: &mut Input) -> Result<Option<Output>> {
    input.advance(
        &mut std::io::empty(),
        ImageDecodeWindow { bytes: 1, steps: 1 },
    )
}

fn complete(input: &mut Input) -> (Output, usize) {
    for polls in 1..100_000 {
        if let Some(value) = advance(input).unwrap() {
            return (value, polls);
        }
    }
    panic!("field construction stopped progressing")
}

fn parse_statement(sql: &str) -> Statement {
    crate::compile(sql).unwrap().remove(0)
}

fn source_field(statement: Statement) -> Source {
    match statement {
        Statement::Update(update) => Source::Assignments(update.assignments),
        Statement::Insert(insert) => match insert.on_conflict {
            Some(conflict) => Source::Conflict(conflict),
            None => Source::Targets(insert.columns),
        },
        Statement::Merge(merge) => Source::Merge(merge.when_clauses),
        _ => panic!("mutation fixture shape"),
    }
}

fn expected_field(plan: CommandPlan) -> (Field, Vec<i64>) {
    let (field, queries) = match plan {
        CommandPlan::Update(update) => (Field::Assignments(update.assignments), update.subqueries),
        CommandPlan::Insert(insert) => {
            let field = match insert.on_conflict {
                Some(conflict) => Field::Conflict(conflict),
                None => Field::Targets(insert.columns),
            };
            (field, insert.subqueries)
        }
        CommandPlan::Merge(merge) => (Field::Merge(merge.when_clauses), merge.subqueries),
        _ => panic!("mutation fixture shape"),
    };
    let queries = queries
        .into_iter()
        .map(|query| {
            let RelationalPlan::QueryBlock(block) = query.root else {
                panic!("scalar fixture query");
            };
            let ScalarExpr::Literal(Value::Int(value)) = block.projections[0].expr else {
                panic!("literal fixture query");
            };
            value
        })
        .collect();
    (field, queries)
}
