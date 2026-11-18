//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::plan::{OwnedScalarExpr, RelationalPlan, UnifiedPlan};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::MemoryBudget,
    CancellationToken,
};

struct Classifier {
    calls: AtomicUsize,
    names: Mutex<Vec<String>>,
}

impl Classifier {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            names: Mutex::new(Vec::new()),
        }
    }
}

impl AggregateClassifier for Classifier {
    fn is_registered_aggregate(&self, name: &str) -> bool {
        name == "custom_aggregate"
    }

    fn is_registered_aggregate_with_control(
        &self,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        control.check()?;
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.names.lock().unwrap().push(name.into());
        if name == "rejected" {
            return Err(SQLError::Internal("aggregate lookup rejected".into()));
        }
        Ok(self.is_registered_aggregate(name))
    }
}

fn block(sql: &str) -> QueryBlockPlan {
    let statement = crate::compile(sql).unwrap().remove(0);
    let UnifiedPlan::Query(query) = UnifiedPlan::lower(statement) else {
        panic!("query");
    };
    let RelationalPlan::QueryBlock(block) = query.root else {
        panic!("block");
    };
    *block
}

fn advance<T: Send + 'static>(
    input: &mut ImageDecoder<T, SQLError>,
) -> Result<Option<T>, SQLError> {
    input.advance(
        &mut std::io::empty(),
        ImageDecodeWindow { bytes: 1, steps: 1 },
    )
}

#[test]
fn native_computation_preserves_aggregate_precedence_and_query_window_boundaries() {
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    for (sql, expected) in [
        ("SELECT 1", ComputePlan::Project),
        ("SELECT row_number() OVER ()", ComputePlan::Window),
        (
            "SELECT sum(x), row_number() OVER () FROM t",
            ComputePlan::Aggregate,
        ),
        ("SELECT 1 GROUP BY 1", ComputePlan::Aggregate),
        ("SELECT 1 HAVING TRUE", ComputePlan::Aggregate),
        ("SELECT (SELECT sum(x) FROM t)", ComputePlan::Project),
        (
            "SELECT row_number() OVER (ORDER BY sum(x)) FROM t",
            ComputePlan::Aggregate,
        ),
        (
            "SELECT opaque(1 ORDER BY row_number() OVER ())",
            ComputePlan::Window,
        ),
        ("SELECT custom_aggregate(x) FROM t", ComputePlan::Aggregate),
    ] {
        let block = block(sql);
        let classifier = Arc::new(Classifier::new());
        assert_eq!(
            std::mem::discriminant(&block.compute_kind(classifier.as_ref())),
            std::mem::discriminant(&expected),
            "{sql}"
        );
        let original = token.clone();
        let destination = native.clone();
        let lookup = Arc::clone(&classifier);
        let mut input = ImageDecoder::new(&work, &token, |mut reader| async move {
            let mut input = reader.input();
            let control = ProductionControl::new(&destination, &original, input.cancellation);
            block
                .compute_kind_from_input(lookup.as_ref(), &control, &mut input)
                .await
        })
        .unwrap();
        for polls in 1..1024 {
            let before = classifier.calls.load(Ordering::Relaxed);
            let result = advance(&mut input).unwrap();
            assert!(classifier.calls.load(Ordering::Relaxed) - before <= 1);
            if let Some(result) = result {
                assert_eq!(
                    std::mem::discriminant(&result),
                    std::mem::discriminant(&expected),
                    "{sql}"
                );
                break;
            }
            assert!(polls < 1023, "computation did not finish");
        }
        assert_eq!((native.used(), work.used()), (0, 0));
    }
}

type Observation = ImageDecoder<(bool, bool), SQLError>;

#[test]
fn native_computation_counts_empty_window_definitions_against_work() {
    let mut block = block("SELECT 1");
    block.windows = (0..2048)
        .map(|_| crate::ast::WindowDefinition {
            name: None,
            inherited: None,
            spec: crate::ir::ScalarWindowSpec {
                definition: None,
                partition_by: Vec::new(),
                order_by: Vec::new(),
                frame: None,
            },
        })
        .collect();
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let original = token.clone();
    let destination = native.clone();
    let mut input = ImageDecoder::new(&work, &token, |mut reader| async move {
        let mut input = reader.input();
        let control = ProductionControl::new(&destination, &original, input.cancellation);
        block
            .compute_kind_from_input(&Classifier::new(), &control, &mut input)
            .await
    })
    .unwrap();
    let mut polls = 0;
    loop {
        polls += 1;
        if let Some(result) = advance(&mut input).unwrap() {
            assert!(matches!(result, ComputePlan::Project));
            break;
        }
        assert!(polls < 4096);
    }
    assert!(polls >= 2048);
    assert_eq!((native.used(), work.used()), (0, 0));
}

fn observe(
    source: Arc<OwnedScalarExpr>,
    native: &MemoryBudget,
    work: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
    classifier: Arc<Classifier>,
) -> Observation {
    let native = native.clone();
    let original = original.clone();
    ImageDecoder::new(work, invoking, |mut reader| async move {
        let mut input = reader.input();
        let control = ProductionControl::new(&native, &original, input.cancellation);
        let aggregate = crate::semantics::aggregates::contains_with_work(
            &source,
            &control,
            &mut input,
            &mut |expression| {
                crate::semantics::aggregates::is_aggregate_with_control(
                    classifier.as_ref(),
                    expression,
                    &control,
                )
            },
        )
        .await?;
        Ok((
            aggregate,
            source
                .contains_window_with_work(&control, &mut input)
                .await?,
        ))
    })
    .unwrap()
}

fn scalar(sql: &str) -> Arc<OwnedScalarExpr> {
    let mut block = block(sql);
    Arc::new(OwnedScalarExpr::from_rewrite_parts(
        block.projections.remove(0).expr,
        None,
    ))
}

#[test]
fn native_aggregate_observation_keeps_pruning_order_and_late_lookup_failures() {
    let native = MemoryBudget::new(1 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    for rejected in [false, true] {
        let source = scalar(if rejected {
            "SELECT ROW(custom_aggregate(hidden(1)), rejected(2))"
        } else {
            "SELECT CASE probe(1) WHEN probe(2) THEN custom_aggregate(hidden(3)) ELSE later(4) END"
        });
        let classifier = Arc::new(Classifier::new());
        let input = observe(
            source,
            &native,
            &work,
            &token,
            &token,
            Arc::clone(&classifier),
        );
        let result = input.read(&mut std::io::empty());
        if rejected {
            assert!(result
                .unwrap_err()
                .to_string()
                .contains("aggregate lookup rejected"));
            assert_eq!(
                *classifier.names.lock().unwrap(),
                ["custom_aggregate", "rejected"]
            );
        } else {
            assert_eq!(result.unwrap(), (true, false));
            assert_eq!(
                *classifier.names.lock().unwrap(),
                ["probe", "probe", "custom_aggregate", "later"]
            );
        }
        assert_eq!((native.used(), work.used()), (0, 0));
    }
}

#[test]
fn native_scalar_observation_bounds_deep_work_and_retires_every_interrupted_prefix() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let mut expression = ScalarExpr::Literal(uqa_core::Value::Int(1));
            for _ in 0..8192 {
                expression = ScalarExpr::Not(Box::new(expression));
            }
            let source = Arc::new(OwnedScalarExpr::from_rewrite_parts(expression, None));
            let native = MemoryBudget::new(2 << 20);
            let work = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            let classifier = Arc::new(Classifier::new());
            let mut input = observe(
                Arc::clone(&source),
                &native,
                &work,
                &token,
                &token,
                Arc::clone(&classifier),
            );
            let mut polls = 0;
            loop {
                polls += 1;
                if let Some(result) = advance(&mut input).unwrap() {
                    assert_eq!(result, (false, false));
                    break;
                }
                assert!(polls < 100_000);
            }
            assert!(polls > 2 * 8192);
            assert!(native.peak() > 0);
            assert_eq!((native.used(), work.used()), (0, 0));
            let sibling = native.reserve(43).unwrap();
            for prefix in [0, 1, 8, 32, 64, 128, 1024] {
                for cancelled in 0..3 {
                    let original = CancellationToken::new();
                    let invoking = CancellationToken::new();
                    let mut input = observe(
                        Arc::clone(&source),
                        &native,
                        &work,
                        &original,
                        &invoking,
                        Arc::clone(&classifier),
                    );
                    for _ in 0..prefix {
                        assert!(advance(&mut input).unwrap().is_none());
                    }
                    if cancelled != 0 {
                        if cancelled == 1 {
                            original.cancel();
                        } else {
                            invoking.cancel();
                        }
                        assert_eq!(advance(&mut input).unwrap_err().sqlstate(), Some("57014"));
                    }
                    drop(input);
                    assert_eq!((native.used(), work.used()), (43, 0));
                }
            }
            let denied = MemoryBudget::new(0);
            let input = observe(
                Arc::clone(&source),
                &denied,
                &work,
                &token,
                &token,
                classifier,
            );
            assert_eq!(
                input.read(&mut std::io::empty()).unwrap_err().sqlstate(),
                Some("53200")
            );
            assert_eq!((denied.used(), work.used()), (0, 0));
            drop(sibling);
            assert_eq!(allocation_counter::measure(|| drop(source)).count_total, 0);
        })
        .unwrap()
        .join()
        .unwrap();
}
