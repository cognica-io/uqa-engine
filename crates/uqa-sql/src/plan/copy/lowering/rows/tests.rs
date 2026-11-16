//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    construction::{self, Work},
    plan::{mutable::values::OwnedRows, ExpressionPlan, NoRegisteredAggregates, OwnedScalarExpr},
    SQLError,
};
use std::sync::Arc;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::MemoryBudget,
    CancellationToken, Value,
};

struct NativeRows<'a>(Work<'a>);

impl Copying<'_> {
    fn lower_rows_with<'s: 'a, 'a, I: ScalarInput<&'s crate::ast::Expr> + Send + 'a>(
        &'a mut self,
        rows: &'s [Vec<Expr>],
        input: &'a mut I,
    ) -> Result<construction::Read<'a, Result<Vec<Vec<ScalarExpr>>>>> {
        Ok(construction::Read::new(
            self.control,
            construct_rows(
                rows,
                input,
                self.control,
                self.fields.as_mut().expect("row records"),
                self.memory.as_mut().expect("row allowance"),
            ),
        )?)
    }
}

impl<'s> ScalarInput<&'s crate::ast::Expr> for NativeRows<'_> {
    async fn checkpoint(&mut self) -> Result<()> {
        self.0.checkpoint().await;
        Ok(())
    }

    async fn scalar(
        &mut self,
        expression: &'s Expr,
        control: ProductionControl<'_>,
    ) -> Result<OwnedScalarExpr> {
        struct Queries;
        impl<'s>
            crate::plan::scalar::ScalarQueryInput<&'s crate::ast::Expr, &'s crate::ast::SelectStmt>
            for Queries
        {
            async fn append(
                &mut self,
                _: &'s crate::ast::SelectStmt,
                _: &ProductionControl<'_>,
            ) -> Result<usize> {
                Err(SQLError::Internal("row query rejected".into()))
            }
        }
        ExpressionPlan::lower_scalar_with_query_input(
            expression,
            &mut Queries,
            &control,
            &mut self.0,
        )?
        .await
    }
}

fn input(
    source: Arc<Vec<Vec<Expr>>>,
    native: &MemoryBudget,
    work: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> ImageDecoder<OwnedRows, SQLError> {
    let native = native.clone();
    let original = original.clone();
    ImageDecoder::new(work, invoking, |mut reader| async move {
        let mut input = reader.input();
        let control = ProductionControl::new(&native, &original, input.cancellation);
        let mut driver = construction::Driver::new(control)?;
        let source = source.as_slice();
        let output = driver.work().submit(move |work| async move {
            let mut copying = Copying::for_rewrite(control).unwrap();
            let rows = copying
                .lower_rows_with(source, &mut NativeRows(work))?
                .await?;
            Ok::<_, SQLError>(OwnedRows {
                rows,
                memory: Some(copying.into_rewrite_memory()),
            })
        })?;
        while driver.advance().await? {
            input.checkpoint().await?;
        }
        construction::receive(output).await
    })
    .unwrap()
}

fn advance(reader: &mut ImageDecoder<OwnedRows, SQLError>) -> Result<Option<OwnedRows>> {
    reader.advance(
        &mut std::io::empty(),
        ImageDecodeWindow { bytes: 1, steps: 1 },
    )
}

fn complete(reader: &mut ImageDecoder<OwnedRows, SQLError>) -> (OwnedRows, usize) {
    for polls in 1..1_000_000 {
        if let Some(rows) = advance(reader).unwrap() {
            return (rows, polls);
        }
    }
    panic!("row construction stopped progressing")
}

#[test]
fn retained_value_rows_share_ordinary_shape_and_independently_movable_cells() {
    let source = Arc::new(vec![
        vec![],
        vec![
            Expr::Literal(Value::Int(7)),
            Expr::Literal(Value::Str("selected".into())),
        ],
        vec![Expr::Array(vec![Expr::Literal(Value::Str(
            "discarded".repeat(2048),
        ))])],
    ]);
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&native, &token, &token);
    let mut copying = Copying::for_rewrite(control).unwrap();
    let expected = copying
        .lower_rows(
            &source,
            &NoRegisteredAggregates,
            &mut QueryArena::new(control, None),
        )
        .unwrap();
    let expected = OwnedRows {
        rows: expected,
        memory: Some(copying.into_rewrite_memory()),
    };
    let ordinary_bytes = native.used();
    let (mut actual, polls) = complete(&mut input(source, &native, &work, &token, &token));
    assert!(polls > 20);
    assert_eq!(actual.rows, expected.rows);
    assert_eq!(work.used(), 0);
    assert_eq!(native.used(), ordinary_bytes * 2);
    drop(expected);
    let selected = actual.take_row(1).nth(1).unwrap();
    drop(actual);
    assert_eq!(
        *selected,
        ScalarExpr::Literal(Value::Str("selected".into()))
    );
    assert_eq!(native.used(), "selected".len());
    drop(selected);
    assert_eq!(native.used(), 0);
}

#[test]
fn retained_value_rows_retire_every_prefix_cancellation_and_denied_destination() {
    let source = Arc::new(vec![
        vec![],
        vec![
            Expr::Literal(Value::Str("long_é🙂".repeat(1024))),
            Expr::Not(Box::new(Expr::Literal(Value::Bool(false)))),
        ],
        vec![Expr::Literal(Value::Int(2))],
    ]);
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let (rows, polls) = complete(&mut input(source.clone(), &native, &work, &token, &token));
    drop(rows);
    let sibling = native.reserve(37).unwrap();
    for prefix in 0..polls - 1 {
        for cancel in 0..3 {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let mut reader = input(source.clone(), &native, &work, &original, &invoking);
            for _ in 0..prefix {
                assert!(advance(&mut reader).unwrap().is_none());
            }
            if cancel != 0 {
                if cancel == 1 {
                    original.cancel();
                } else {
                    invoking.cancel();
                }
                assert_eq!(advance(&mut reader).unwrap_err().sqlstate(), Some("57014"));
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
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("53200"));
        assert_eq!((denied.used(), work.used()), (0, 0));
        assert!(denied.peak() <= bytes);
    }
    let crate::ast::Statement::Select(mut query) =
        crate::compile("SELECT (SELECT 1)").unwrap().remove(0)
    else {
        panic!("query fixture")
    };
    let rejected = Arc::new(vec![
        vec![Expr::Literal(Value::Str("completed".repeat(4096)))],
        vec![query.projections.remove(0).expr],
    ]);
    let error = input(rejected, &native, &work, &token, &token)
        .read(&mut std::io::empty())
        .unwrap_err();
    assert!(error.to_string().contains("row query rejected"));
    assert_eq!((native.used(), work.used()), (0, 0));
}

#[test]
fn retained_value_rows_count_empty_rows_against_each_work_window() {
    let mut rows = vec![vec![]; 2048];
    rows.push(vec![Expr::Literal(Value::Str("last".into()))]);
    let source = Arc::new(rows);
    let native = MemoryBudget::new(4 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let mut reader = input(source, &native, &work, &token, &token);
    for _ in 0..2048 {
        assert!(advance(&mut reader).unwrap().is_none());
    }
    let (mut rows, _) = complete(&mut reader);
    assert_eq!(rows.rows.len(), 2049);
    let selected = rows.take_row(2048).next().unwrap();
    drop(rows);
    assert_eq!(native.used(), 4);
    drop(selected);
    assert_eq!((native.used(), work.used()), (0, 0));
}

#[test]
fn retained_value_rows_release_completed_and_partial_deep_cells_on_a_small_stack() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let mut source = Expr::Literal(Value::Str("leaf".into()));
            for _ in 0..8192 {
                source = Expr::Not(Box::new(source));
            }
            let source = Arc::new(vec![vec![source], vec![Expr::Literal(Value::Int(1))]]);
            let native = MemoryBudget::new(32 << 20);
            let work = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let (mut rows, polls) =
                complete(&mut input(source.clone(), &native, &work, &token, &token));
            assert!(polls > 16_384);
            let row = rows.take_row(0);
            drop(rows);
            assert_eq!(allocation_counter::measure(|| drop(row)).count_total, 0);
            assert_eq!(native.used(), 0);
            let (rows, _) = complete(&mut input(source.clone(), &native, &work, &token, &token));
            assert_eq!(allocation_counter::measure(|| drop(rows)).count_total, 0);
            let mut reader = input(source, &native, &work, &token, &token);
            for _ in 0..polls - 2 {
                assert!(advance(&mut reader).unwrap().is_none());
            }
            assert_eq!(allocation_counter::measure(|| drop(reader)).count_total, 0);
            assert_eq!((native.used(), work.used()), (0, 0));
        })
        .unwrap()
        .join()
        .unwrap();
}
