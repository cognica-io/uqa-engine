//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::{expressions, projections, windows, Result};
use super::*;
use crate::{
    ast::{OrderBy, Projection, SelectStmt, Statement, WindowDefinition},
    construction::{self, Work},
    plan::{
        mutable::memory::RewriteMemory,
        retirement::{Pending, Retire},
        ExpressionPlan, OrderPlan, ProjectionPlan, RelationalPlan, UnifiedPlan,
    },
    SQLError, ScalarExpr, ScalarWindowSpec,
};

mod blocks;
mod sources;
use std::sync::Arc;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::{BudgetedVec, MemoryBudget, Produced},
    CancellationToken, Value,
};

enum Source {
    Block(Box<SelectStmt>),
    From(crate::ast::FromClause),
    Values(Vec<Expr>),
    Orders(Vec<OrderBy>),
    Optional(Option<Expr>, bool),
    Projections(Vec<Projection>),
    Windows(Vec<WindowDefinition>),
}

#[derive(Debug)]
enum Field {
    Block(Box<crate::plan::retirement::construction::Prefix<crate::plan::QueryBlockPlan>>),
    From(crate::plan::SourcePlan),
    Values(Vec<ScalarExpr>),
    Orders(Vec<OrderPlan>),
    Optional(Option<ScalarExpr>),
    Boxed(Option<Box<ScalarExpr>>),
    Projections(Vec<ProjectionPlan>),
    Windows(Vec<WindowDefinition<ScalarWindowSpec>>),
}

impl PartialEq for Field {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Block(left), Self::Block(right)) => {
                serde_json::to_value(&***left).unwrap() == serde_json::to_value(&***right).unwrap()
            }
            (Self::From(left), Self::From(right)) => {
                serde_json::to_value(left).unwrap() == serde_json::to_value(right).unwrap()
            }
            (Self::Values(left), Self::Values(right)) => left == right,
            (Self::Optional(left), Self::Optional(right)) => left == right,
            (Self::Boxed(left), Self::Boxed(right)) => left == right,
            (Self::Orders(left), Self::Orders(right)) => left
                .iter()
                .map(|order| (&order.expr, order.descending, order.nulls))
                .eq(right
                    .iter()
                    .map(|order| (&order.expr, order.descending, order.nulls))),
            (Self::Windows(left), Self::Windows(right)) => left == right,
            (Self::Projections(left), Self::Projections(right)) => left
                .iter()
                .map(|projection| (&projection.expr, &projection.alias))
                .eq(right
                    .iter()
                    .map(|projection| (&projection.expr, &projection.alias))),
            _ => false,
        }
    }
}

impl Retire for Field {
    fn retire(&mut self) {
        match self {
            Self::Block(_) => {}
            Self::From(source) => source.retire(),
            Self::Values(values) => values.retire(),
            Self::Orders(values) => values.retire(),
            Self::Optional(value) => value.retire(),
            Self::Boxed(value) => value.retire(),
            Self::Projections(values) => values.retire(),
            Self::Windows(values) => values.retire(),
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
            return Err(SQLError::Internal("rejected scalar query".into()));
        }
        let index = self.values.len();
        self.values.push(value)?;
        Ok(index)
    }
}

struct Native<'a> {
    work: Work<'a>,
    queries: Option<Queries<'a>>,
}

impl<'s: 'a, 'a> ScalarInput<&'s Expr> for Native<'a> {
    async fn checkpoint(&mut self) -> Result<()> {
        self.work.checkpoint().await;
        Ok(())
    }

    async fn scalar(
        &mut self,
        expression: &'s Expr,
        control: ProductionControl<'_>,
    ) -> Result<OwnedScalarExpr> {
        let mut queries = self.queries.take().expect("active scalar query owner");
        let control = control.capture();
        let output = self.work.submit(move |mut work| async move {
            let control = control.control();
            let value = ExpressionPlan::lower_scalar_with_query_input(
                expression,
                &mut queries,
                &control,
                &mut work,
            )?
            .await?;
            Ok::<_, SQLError>((value, queries))
        })?;
        let (value, queries) = construction::receive(output).await?;
        self.queries = Some(queries);
        Ok(value)
    }
}

impl<'s> TextInput<&'s str> for Native<'_> {
    async fn text(
        &mut self,
        source: &'s str,
        control: ProductionControl<'_>,
    ) -> Result<Produced<String>> {
        let copied = crate::catalog::work::copy_text(&mut self.work, source, &control).await?;
        if source == "reject_source_alias" {
            return Err(SQLError::Internal("rejected source alias".into()));
        }
        Ok(copied)
    }
}

impl<'t, 'b> SourceMetadataInput<&'t crate::ast::ColumnType, &'b crate::ast::FunctionBinding>
    for Native<'_>
{
    async fn ty(
        &mut self,
        source: &'t crate::ast::ColumnType,
        control: ProductionControl<'_>,
    ) -> Result<Produced<crate::ast::ColumnType>> {
        source.clone_with_input_work(&control, &mut self.work).await
    }
    async fn binding(
        &mut self,
        source: &'b crate::ast::FunctionBinding,
        control: ProductionControl<'_>,
    ) -> Result<Produced<crate::ast::FunctionBinding>> {
        source.clone_with_input_work(&control, &mut self.work).await
    }
}

fn retained_field<
    's: 'a,
    'c: 'a,
    'a,
    I: super::super::source::leaf::Input<'s> + super::super::query::block::Input<'s, 'c> + Send + 'a,
>(
    source: &'s Source,
    copying: &'a mut Copying<'c>,
    input: &'a mut I,
) -> Result<construction::Read<'a, Result<Pending<Field>>>> {
    Ok(construction::Read::new(copying.control, async move {
        Ok(Pending::new(match source {
            Source::Block(source) => {
                Field::Block(blocks::block_input(source, copying, input)?.await?)
            }
            Source::From(source) => Field::From(
                super::super::source::leaf::construct(source, copying, input)
                    .await?
                    .take(),
            ),
            Source::Values(values) => Field::Values(
                scalars::construct_scalars(values, copying, input, |value| value, |_, value| value)
                    .await?
                    .take(),
            ),
            Source::Orders(orders) => Field::Orders(
                scalars::construct_scalars(
                    orders,
                    copying,
                    input,
                    |order| &order.expr,
                    |order, expr| OrderPlan {
                        expr,
                        descending: order.descending,
                        nulls: order.nulls,
                    },
                )
                .await?
                .take(),
            ),
            Source::Projections(values) => Field::Projections(
                projections::construct_projections(values, copying, input)
                    .await?
                    .take(),
            ),
            Source::Windows(values) => Field::Windows(
                windows::construct_windows(values, copying, input)
                    .await?
                    .take(),
            ),
            Source::Optional(value, false) => Field::Optional(
                expressions::construct_optional_scalar(value.as_ref(), copying, input)
                    .await?
                    .take(),
            ),
            Source::Optional(value, true) => Field::Boxed(
                expressions::construct_optional_scalar_box(value.as_ref(), copying, input)
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
                queries: Some(Queries {
                    work: work.clone(),
                    values: BudgetedVec::new(native),
                }),
                work,
            };
            let mut copying = Copying::for_rewrite(control).unwrap();
            let field = retained_field(source, &mut copying, &mut input)?.await?;
            Ok::<_, SQLError>((
                field,
                copying.into_rewrite_memory(),
                input.queries.expect("completed query callbacks").values,
            ))
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

#[test]
fn retained_query_scalar_fields_preserve_slots_ordering_flags_and_optional_shape() {
    let statement = crate::compile("SELECT 1 GROUP BY (SELECT 3), (SELECT 4) ORDER BY (SELECT 5) DESC NULLS FIRST, (SELECT 6) NULLS LAST LIMIT (SELECT 7)").unwrap().remove(0);
    let Statement::Select(source) = &statement else {
        panic!("query fixture");
    };
    let UnifiedPlan::Query(expected) = UnifiedPlan::lower(statement.clone()) else {
        panic!("query fixture");
    };
    let RelationalPlan::QueryBlock(block) = expected.root else {
        panic!("block fixture");
    };
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    for (source, expected, queries) in [
        (
            Source::Values(source.group_by.clone()),
            Field::Values(block.group_by),
            vec![3, 4],
        ),
        (
            Source::Orders(source.order_by.clone()),
            Field::Orders(vec![
                OrderPlan {
                    expr: ScalarExpr::ScalarSubquery(0),
                    descending: true,
                    nulls: source.order_by[0].nulls,
                },
                OrderPlan {
                    expr: ScalarExpr::ScalarSubquery(1),
                    descending: false,
                    nulls: source.order_by[1].nulls,
                },
            ]),
            vec![5, 6],
        ),
        (
            Source::Optional(source.limit.clone(), false),
            Field::Optional(Some(ScalarExpr::ScalarSubquery(0))),
            vec![7],
        ),
        (
            Source::Optional(source.limit.clone(), true),
            Field::Boxed(Some(Box::new(ScalarExpr::ScalarSubquery(0)))),
            vec![7],
        ),
        (Source::Optional(None, false), Field::Optional(None), vec![]),
        (Source::Optional(None, true), Field::Boxed(None), vec![]),
        (Source::Values(vec![]), Field::Values(vec![]), vec![]),
        (Source::Orders(vec![]), Field::Orders(vec![]), vec![]),
    ] {
        let ((value, memory, actual_queries), polls) =
            complete(&mut input(Arc::new(source), &native, &work, &token, &token));
        assert_eq!(*value, expected);
        assert_eq!(&*actual_queries, queries.as_slice());
        assert!(polls > 1);
        assert_eq!(work.used(), 0);
        assert_eq!(allocation_counter::measure(|| drop(value)).count_total, 0);
        drop((memory, actual_queries));
        assert_eq!(native.used(), 0);
    }
}

#[test]
fn retained_query_scalar_fields_retire_cancelled_prefixes_and_denied_destinations() {
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    for source in [
        Source::Values(vec![
            Expr::Literal(Value::Str("é🙂".repeat(4096))),
            Expr::Literal(Value::Int(4)),
        ]),
        Source::Optional(Some(Expr::Literal(Value::Str("é🙂".repeat(4096)))), false),
        Source::Optional(Some(Expr::Literal(Value::Str("é🙂".repeat(4096)))), true),
    ] {
        let source = Arc::new(source);
        let (value, polls) = complete(&mut input(source.clone(), &native, &work, &token, &token));
        drop(value);
        let sibling = native.reserve(37).unwrap();
        for prefix in 0..polls - 1 {
            for cancellation in 0..3 {
                let original = CancellationToken::new();
                let invoking = CancellationToken::new();
                let mut reader = input(source.clone(), &native, &work, &original, &invoking);
                for _ in 0..prefix {
                    assert!(advance(&mut reader).unwrap().is_none());
                }
                if cancellation != 0 {
                    if cancellation == 1 {
                        original.cancel();
                    } else {
                        invoking.cancel();
                    }
                    assert_eq!(
                        advance(&mut reader).err().unwrap().sqlstate(),
                        Some("57014")
                    );
                    assert_eq!(
                        advance(&mut reader).err().unwrap().sqlstate(),
                        Some("XX001")
                    );
                }
                drop(reader);
                assert_eq!((native.used(), work.used()), (37, 0));
            }
        }
        drop(sibling);
        for bytes in [0, 1, 1024, 4096, 8192] {
            let denied = MemoryBudget::new(bytes);
            let error = input(source.clone(), &denied, &work, &token, &token)
                .read(&mut std::io::empty())
                .err()
                .unwrap();
            assert_eq!(error.sqlstate(), Some("53200"));
            assert_eq!((denied.used(), work.used()), (0, 0));
            assert!(denied.peak() <= bytes);
        }
    }
}

#[test]
fn retained_query_scalar_vectors_retire_deep_prefixes_after_late_query_failure() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let Statement::Select(mut source) =
                crate::compile("SELECT (SELECT 99)").unwrap().remove(0)
            else {
                panic!("query fixture");
            };
            let mut deep = Expr::Literal(Value::Str("leaf".into()));
            for _ in 0..8192 {
                deep = Expr::Not(Box::new(deep));
            }
            let source = Arc::new(Source::Values(vec![
                deep,
                source.projections.remove(0).expr,
            ]));
            let native = MemoryBudget::new(32 << 20);
            let work = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let error = input(source, &native, &work, &token, &token)
                .read(&mut std::io::empty())
                .err()
                .unwrap();
            assert_eq!(error.to_string(), "internal error: rejected scalar query");
            assert_eq!((native.used(), work.used()), (0, 0));
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn retained_query_named_fields_preserve_query_order_inheritance_and_owned_names() {
    let native = MemoryBudget::new(2 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    for sql in [
        "SELECT (SELECT 2) AS first, (SELECT 3) AS second",
        "SELECT 1 WINDOW base AS (PARTITION BY (SELECT 5)), child AS (base ORDER BY (SELECT 6) DESC NULLS FIRST)",
    ] {
        let Statement::Select(mut statement) = crate::compile(sql).unwrap().remove(0) else { panic!("query fixture"); };
        let projection = statement.windows.is_empty();
        if projection { statement.projections[0].alias = Some("alias_é🙂".repeat(4096)); }
        else { statement.windows[0].name = Some("window_é🙂".repeat(4096)); }
        let UnifiedPlan::Query(ordinary) = UnifiedPlan::lower(Statement::Select(statement.clone())) else { panic!("query fixture"); };
        let RelationalPlan::QueryBlock(block) = ordinary.root else { panic!("query block"); };
        let (source, expected, expected_queries) = if projection {
            (Source::Projections(statement.projections), Field::Projections(block.projections), vec![2,3])
        } else {
            (Source::Windows(statement.windows), Field::Windows(block.windows), vec![5,6])
        };
        let source = Arc::new(source);
        let ((value, memory, queries), polls) = complete(&mut input(source.clone(), &native, &work, &token, &token));
        drop(source);
        assert_eq!(*value, expected);
        assert_eq!(&*queries, expected_queries.as_slice());
        assert!(polls > 8);
        assert_eq!(allocation_counter::measure(|| drop(value)).count_total, 0);
        drop((memory, queries));
        assert_eq!((native.used(), work.used()), (0, 0));
    }
}

#[test]
fn retained_query_named_vectors_retire_every_cancelled_or_denied_prefix() {
    let native = MemoryBudget::new(8 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let Statement::Select(mut query) =
        crate::compile("SELECT 1 WINDOW base AS (PARTITION BY 'leaf')")
            .unwrap()
            .remove(0)
    else {
        panic!("query fixture");
    };
    query.windows[0].name = Some("window_é🙂".repeat(4096));
    for source in [
        Source::Projections(vec![Projection {
            expr: Expr::Literal(Value::Str("leaf".into())),
            alias: Some("alias_é🙂".repeat(4096)),
        }]),
        Source::Windows(query.windows),
    ] {
        let source = Arc::new(source);
        let (value, polls) = complete(&mut input(source.clone(), &native, &work, &token, &token));
        drop(value);
        let sibling = native.reserve(37).unwrap();
        for prefix in 0..polls - 1 {
            for cancellation in 0..3 {
                let original = CancellationToken::new();
                let invoking = CancellationToken::new();
                let mut reader = input(source.clone(), &native, &work, &original, &invoking);
                for _ in 0..prefix {
                    assert!(advance(&mut reader).unwrap().is_none());
                }
                if cancellation != 0 {
                    if cancellation == 1 {
                        original.cancel();
                    } else {
                        invoking.cancel();
                    }
                    assert_eq!(
                        advance(&mut reader).err().unwrap().sqlstate(),
                        Some("57014")
                    );
                    assert_eq!(
                        advance(&mut reader).err().unwrap().sqlstate(),
                        Some("XX001")
                    );
                }
                drop(reader);
                assert_eq!((native.used(), work.used()), (37, 0));
            }
        }
        drop(sibling);
        for bytes in [0, 1, 1024, 4096, 8192] {
            let denied = MemoryBudget::new(bytes);
            assert_eq!(
                input(source.clone(), &denied, &work, &token, &token)
                    .read(&mut std::io::empty())
                    .err()
                    .unwrap()
                    .sqlstate(),
                Some("53200")
            );
            assert_eq!((denied.used(), work.used()), (0, 0));
        }
    }
}

#[test]
fn retained_query_named_vectors_bound_empty_and_wide_work() {
    let native = MemoryBudget::new(8 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    for count in [0, 2048] {
        let Statement::Select(query) = crate::compile("SELECT 1 WINDOW base AS ()")
            .unwrap()
            .remove(0)
        else {
            panic!("query fixture");
        };
        for source in [
            Source::Projections(vec![
                Projection {
                    expr: Expr::Literal(Value::Int(7)),
                    alias: None
                };
                count
            ]),
            Source::Windows(vec![query.windows[0].clone(); count]),
        ] {
            let (value, polls) =
                complete(&mut input(Arc::new(source), &native, &work, &token, &token));
            assert!(polls > count + 1);
            match &*value.0 {
                Field::Projections(values) => {
                    assert_eq!(values.len(), count);
                    assert!(
                        values
                            .iter()
                            .all(|p| p.expr == ScalarExpr::Literal(Value::Int(7))
                                && p.alias.is_none())
                    );
                }
                Field::Windows(values) => {
                    assert_eq!(values.len(), count);
                    assert!(values.iter().all(|w| w.spec.partition_by.is_empty()
                        && w.spec.order_by.is_empty()
                        && w.spec.frame.is_none()));
                }
                _ => panic!("named vector"),
            }
            drop(value);
            assert_eq!((native.used(), work.used()), (0, 0));
        }
    }
}
