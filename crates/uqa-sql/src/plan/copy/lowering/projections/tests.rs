//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::Expr,
    construction::{self, Work},
    plan::{retirement::Pending, ExpressionPlan, NoRegisteredAggregates, OwnedScalarExpr},
    SQLError, ScalarExpr,
};
use std::sync::Arc;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::{MemoryBudget, Produced},
    CancellationToken, Value,
};

struct NativeProjection<'a>(Work<'a>);

impl<'s> ScalarInput<&'s crate::ast::Expr> for NativeProjection<'_> {
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
                Err(SQLError::Internal("projection query rejected".into()))
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

impl<'s> TextInput<&'s str> for NativeProjection<'_> {
    async fn text(
        &mut self,
        source: &'s str,
        control: ProductionControl<'_>,
    ) -> Result<Produced<String>> {
        crate::catalog::work::copy_text(&mut self.0, source, &control).await
    }
}

fn retained_projection<
    's: 'a,
    'a,
    I: ScalarInput<&'s crate::ast::Expr> + TextInput<&'s str> + Send + 'a,
>(
    source: &'s Projection,
    control: ProductionControl<'a>,
    input: &'a mut I,
) -> Result<construction::Read<'a, Result<OwnedProjection>>> {
    Ok(construction::Read::new(
        control,
        construct_projection(source, control, input),
    )?)
}

fn input(
    source: Arc<Projection>,
    native: &MemoryBudget,
    work: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> ImageDecoder<OwnedProjection, SQLError> {
    let native = native.clone();
    let original = original.clone();
    ImageDecoder::new(work, invoking, |mut reader| async move {
        let mut input = reader.input();
        let control = ProductionControl::new(&native, &original, input.cancellation);
        let mut driver = construction::Driver::new(control)?;
        let source = source.as_ref();
        let output = driver.work().submit(move |work| async move {
            retained_projection(source, control, &mut NativeProjection(work))?.await
        })?;
        while driver.advance().await? {
            input.checkpoint().await?;
        }
        construction::receive(output).await
    })
    .unwrap()
}

fn advance(
    reader: &mut ImageDecoder<OwnedProjection, SQLError>,
) -> Result<Option<OwnedProjection>> {
    reader.advance(
        &mut std::io::empty(),
        ImageDecodeWindow { bytes: 1, steps: 1 },
    )
}

fn complete(reader: &mut ImageDecoder<OwnedProjection, SQLError>) -> (OwnedProjection, usize) {
    for polls in 1..100_000 {
        if let Some(projection) = advance(reader).unwrap() {
            return (projection, polls);
        }
    }
    panic!("projection construction stopped progressing")
}

#[test]
fn retained_projection_preserves_alias_bytes_order_and_independent_scalar_ownership() {
    let source = Arc::new(Projection {
        expr: Expr::Literal(Value::Str("value".into())),
        alias: Some("alias_é🙂".repeat(4096)),
    });
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&native, &token, &token);
    let mut copying = Copying::for_rewrite(control).unwrap();
    let ordinary = Pending::new(
        copying
            .lower_projection(
                &source,
                &NoRegisteredAggregates,
                &mut QueryArena::new(control, None),
            )
            .unwrap(),
    );
    let ordinary_memory = copying.into_rewrite_memory();
    let (actual, polls) = complete(&mut input(source.clone(), &native, &work, &token, &token));
    assert!(polls > source.alias.as_ref().unwrap().len() / 4096);
    drop(source);
    assert_eq!(*actual.expression, ordinary.expr);
    assert_eq!(
        actual.alias.as_deref().map(String::as_str),
        ordinary.alias.as_deref()
    );
    assert_eq!(work.used(), 0);
    drop(ordinary);
    drop(ordinary_memory);
    drop(actual.alias);
    assert_eq!(native.used(), "value".len());
    assert_eq!(
        *actual.expression,
        ScalarExpr::Literal(Value::Str("value".into()))
    );
    drop(actual.expression);
    assert_eq!(native.used(), 0);
}

#[test]
fn retained_projection_retires_each_suspended_prefix_and_denied_alias() {
    let source = Arc::new(Projection {
        expr: Expr::Array(vec![Expr::Literal(Value::Str("cell".repeat(1024)))]),
        alias: Some("alias_é🙂".repeat(4096)),
    });
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let (projection, polls) = complete(&mut input(source.clone(), &native, &work, &token, &token));
    drop(projection);
    let sibling = native.reserve(33).unwrap();
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
                assert_eq!(
                    advance(&mut reader)
                        .err()
                        .expect("cancelled projection")
                        .sqlstate(),
                    Some("57014")
                );
            }
            drop(reader);
            assert_eq!((native.used(), work.used()), (33, 0));
        }
    }
    drop(sibling);
    for bytes in [0, 1, 1024, 4096, 8192, 16384] {
        let denied = MemoryBudget::new(bytes);
        let error = input(source.clone(), &denied, &work, &token, &token)
            .read(&mut std::io::empty())
            .err()
            .expect("denied projection");
        assert_eq!(error.sqlstate(), Some("53200"));
        assert_eq!((denied.used(), work.used()), (0, 0));
        assert!(denied.peak() <= bytes);
    }
}

#[test]
fn projection_destination_admission_keeps_deep_scalars_owned_until_alias_transfer() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let mut expression = Expr::Literal(Value::Str("leaf".into()));
            for _ in 0..8192 {
                expression = Expr::Not(Box::new(expression));
            }
            let source = Arc::new(Projection {
                expr: expression,
                alias: Some("alias".into()),
            });
            let limit = 32 << 20;
            let native = MemoryBudget::new(limit);
            let work = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let control = ProductionControl::new(&native, &token, &token);
            let (projection, _) =
                complete(&mut input(source.clone(), &native, &work, &token, &token));
            let spare = size_of::<crate::plan::mutable::memory::RewriteField>();
            let sibling_bytes = limit - native.used() - spare;
            let sibling = native.reserve(sibling_bytes).unwrap();
            let mut copying = Copying::for_rewrite(control).unwrap();
            let error = copying.take_projection(projection).unwrap_err();
            assert_eq!(error.sqlstate(), Some("53200"));
            drop(copying);
            assert_eq!(native.used(), sibling_bytes);
            drop(sibling);
            let (projection, _) = complete(&mut input(source, &native, &work, &token, &token));
            let mut copying = Copying::for_rewrite(control).unwrap();
            let projection = Pending::new(copying.take_projection(projection).unwrap());
            let memory = copying.into_rewrite_memory();
            assert_eq!(
                allocation_counter::measure(|| drop(projection)).count_total,
                0
            );
            drop(memory);
            assert_eq!((native.used(), work.used()), (0, 0));
        })
        .unwrap()
        .join()
        .unwrap();
}
