//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{SelectStmt, Statement},
    construction::{self, Work},
    plan::{
        mutable::memory::RewriteMemory, ExpressionPlan, OwnedScalarExpr, RelationalPlan,
        UnifiedPlan,
    },
    SQLError,
};
use std::sync::Arc;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::{MemoryBudget, ProductionControl},
    CancellationToken, Value,
};

struct Queries<'a> {
    work: Work<'a>,
    values: BudgetedVec<i64>,
}

impl<'s> crate::plan::scalar::ScalarQueryInput<&'s crate::ast::Expr, &'s crate::ast::SelectStmt>
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
            panic!("literal fixture query");
        };
        if value == 99 {
            return Err(SQLError::Internal("rejected window query".into()));
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

impl<'s> ScalarInput<&'s crate::ast::Expr> for Native<'_> {
    async fn checkpoint(&mut self) -> Result<()> {
        self.work.checkpoint().await;
        Ok(())
    }

    async fn scalar(
        &mut self,
        source: &'s Expr,
        control: ProductionControl<'_>,
    ) -> Result<OwnedScalarExpr> {
        ExpressionPlan::lower_scalar_with_query_input(
            source,
            &mut self.queries,
            &control,
            &mut self.work,
        )?
        .await
    }
}

type Output = (Pending<ScalarWindowSpec>, RewriteMemory, BudgetedVec<i64>);
type Input = ImageDecoder<Output, SQLError>;

fn retained_window<'s: 'a, 'c: 'a, 'a, I: ScalarInput<&'s crate::ast::Expr> + Send + 'a>(
    source: &'s WindowSpec,
    copying: &'a mut Copying<'c>,
    input: &'a mut I,
) -> Result<construction::Read<'a, Result<Pending<ScalarWindowSpec>>>> {
    Ok(construction::Read::new(
        copying.control,
        construct_window(source, copying, input),
    )?)
}

fn input(
    source: Arc<WindowSpec>,
    native: &MemoryBudget,
    work: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> Input {
    let native = native.clone();
    let original = original.clone();
    ImageDecoder::new(work, invoking, |mut reader| async move {
        let mut input = reader.input();
        let control = ProductionControl::new(&native, &original, input.cancellation);
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
            let value = retained_window(source, &mut copying, &mut input)?.await?;
            Ok::<_, SQLError>((value, copying.into_rewrite_memory(), input.queries.values))
        })?;
        while driver.advance().await? {
            input.checkpoint().await?;
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
    panic!("window construction stopped progressing")
}

fn source(sql: &str) -> (WindowSpec, ScalarWindowSpec) {
    let statement = crate::compile(sql).unwrap().remove(0);
    let UnifiedPlan::Query(mut expected) = UnifiedPlan::lower(statement.clone()) else {
        panic!("fixture query");
    };
    let RelationalPlan::QueryBlock(block) = &mut expected.root else {
        panic!("fixture block");
    };
    let Statement::Select(mut statement) = statement else {
        panic!("fixture syntax");
    };
    (
        statement.windows.remove(0).spec,
        block.windows.remove(0).spec,
    )
}

#[test]
fn retained_window_definitions_preserve_query_order_frame_flags_and_owned_payloads() {
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    for sql in [
        "SELECT 1 WINDOW w AS ()",
        "SELECT 1 WINDOW w AS (PARTITION BY '한🙂', ARRAY[1,2] ORDER BY 3 DESC NULLS FIRST)",
        "SELECT 1 WINDOW w AS (ORDER BY 1 RANGE BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW EXCLUDE GROUP)",
        "SELECT 1 WINDOW w AS (ORDER BY 1 GROUPS BETWEEN CURRENT ROW AND UNBOUNDED FOLLOWING EXCLUDE TIES)",
        "SELECT 1 WINDOW w AS (ORDER BY 1 ROWS 2 PRECEDING)",
        "SELECT 1 WINDOW w AS (PARTITION BY (SELECT 1) ORDER BY (SELECT 2) ROWS BETWEEN (SELECT 3) PRECEDING AND (SELECT 4) FOLLOWING EXCLUDE CURRENT ROW)",
    ] {
        let (source, expected) = source(sql);
        let ((actual, memory, queries), polls) =
            complete(&mut input(Arc::new(source), &native, &work, &token, &token));
        assert_eq!(*actual, expected, "{sql}");
        assert!(polls > 1);
        assert_eq!(work.used(), 0);
        if sql.contains("SELECT 4") {
            assert_eq!(&*queries, &[1, 2, 3, 4]);
        } else {
            assert!(queries.is_empty());
        }
        assert_eq!(allocation_counter::measure(|| drop(actual)).count_total, 0);
        drop((memory, queries));
        assert_eq!(native.used(), 0);
    }
}

#[test]
fn retained_window_definitions_retire_every_prefix_and_denied_field_admission() {
    let (mut source, _) = source("SELECT 1 WINDOW w AS (PARTITION BY 'p' ORDER BY 1 ROWS BETWEEN 2 PRECEDING AND 3 FOLLOWING)");
    source.partition_by[0] = Expr::Literal(Value::Str("payload_é🙂".repeat(1024)));
    let source = Arc::new(source);
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
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
            .expect("denied window payload");
        assert_eq!(error.sqlstate(), Some("53200"));
        assert_eq!((denied.used(), work.used()), (0, 0));
        assert!(denied.peak() <= bytes);
    }
}

#[test]
fn retained_window_definitions_bound_wide_partition_and_order_work() {
    let (mut source, _) = source("SELECT 1 WINDOW w AS (ORDER BY 1)");
    source.partition_by = (0..2048)
        .map(|value| Expr::Literal(Value::Int(value)))
        .collect();
    source.order_by = vec![source.order_by[0].clone(); 2048];
    let native = MemoryBudget::new(16 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let ((value, memory, queries), polls) =
        complete(&mut input(Arc::new(source), &native, &work, &token, &token));
    assert!(polls > 4096);
    assert_eq!(value.partition_by.len(), 2048);
    assert_eq!(value.order_by.len(), 2048);
    for (index, value) in value.partition_by.iter().enumerate() {
        assert_eq!(*value, ScalarExpr::Literal(Value::Int(index as i64)));
    }
    drop((value, memory, queries));
    assert_eq!((native.used(), work.used()), (0, 0));
}

#[test]
fn retained_window_definitions_retire_deep_partial_fields_after_late_query_failure() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let (mut source, _) =
                source("SELECT 1 WINDOW w AS (PARTITION BY 1 ORDER BY (SELECT 99))");
            let mut expression = Expr::Literal(Value::Str("leaf".into()));
            for _ in 0..8192 {
                expression = Expr::Not(Box::new(expression));
            }
            source.partition_by[0] = expression;
            let native = MemoryBudget::new(32 << 20);
            let work = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let mut reader = input(Arc::new(source), &native, &work, &token, &token);
            let error = loop {
                match advance(&mut reader) {
                    Ok(None) => {}
                    Err(error) => break error,
                    Ok(Some(_)) => panic!("late query must reject the window"),
                }
            };
            assert_eq!(error.to_string(), "internal error: rejected window query");
            assert_eq!((native.used(), work.used()), (0, 0));
            drop(reader);
        })
        .unwrap()
        .join()
        .unwrap();
}
