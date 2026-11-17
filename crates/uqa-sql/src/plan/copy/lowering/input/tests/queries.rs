//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::plan::{AggregateClassifier, NoRegisteredAggregates, PlanRef, RetainedExecutablePlan};

mod ctes;
mod edits;
mod entry;
mod mutations;
mod normalization;
mod trees;

struct Aggregates;
impl AggregateClassifier for Aggregates {
    fn is_registered_aggregate(&self, _: &str) -> bool {
        false
    }
    fn is_registered_aggregate_with_control(
        &self,
        name: &str,
        control: &ProductionControl<'_>,
    ) -> Result<bool> {
        control.check()?;
        if name == "reject_function" {
            return Err(SQLError::Internal(
                "rejected aggregate classification".into(),
            ));
        }
        Ok(false)
    }
}

type PlanOutput = RetainedExecutablePlan;
type PlanInput = ImageDecoder<PlanOutput, SQLError>;

fn replace(expression: &Expr) -> Option<usize> {
    matches!(expression, Expr::Not(_)).then_some(7)
}

fn input(
    source: Arc<Statement>,
    native: &MemoryBudget,
    work: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
    parameters: bool,
) -> PlanInput {
    let native = native.clone();
    let original = original.clone();
    ImageDecoder::new(work, invoking, |mut reader| async move {
        let mut channel = reader.input();
        let control = ProductionControl::new(&native, &original, channel.cancellation);
        UnifiedPlan::lower_executable_with_parameters_from_input(
            source.as_ref(),
            &Aggregates,
            &control,
            parameters.then_some(&replace),
            &mut channel,
        )
        .await
    })
    .unwrap()
}

fn json(plan: PlanRef<'_>) -> serde_json::Value {
    match plan {
        PlanRef::Query(query) => serde_json::to_value(query).unwrap(),
        PlanRef::Command(command) => serde_json::to_value(command).unwrap(),
    }
}

fn advance(reader: &mut PlanInput) -> Result<Option<PlanOutput>> {
    reader.advance(
        &mut std::io::empty(),
        ImageDecodeWindow { bytes: 1, steps: 1 },
    )
}

fn complete(reader: &mut PlanInput) -> (PlanOutput, usize) {
    for polls in 1..1_000_000 {
        if let Some(output) = advance(reader).unwrap() {
            return (output, polls);
        }
    }
    panic!("query construction stopped progressing")
}

fn statement(sql: &str) -> Statement {
    crate::compile(sql).unwrap().remove(0)
}

#[test]
fn retained_enclosing_queries_construct_real_arenas_sets_values_and_parameter_scopes() {
    let mut sources = [
        "SELECT 1",
        "SELECT (SELECT (SELECT 1)), (SELECT 2) IN (SELECT (SELECT 3)), EXISTS (SELECT 4)",
        "SELECT (SELECT 1) FROM (VALUES ((SELECT 2))) AS v(n) WHERE (SELECT true) ORDER BY (SELECT 3) LIMIT (SELECT 4)",
        "VALUES ((SELECT 1), (SELECT (SELECT 2))), ((SELECT 3), NULL)",
        "SELECT (SELECT 1) UNION ALL SELECT (SELECT 2) ORDER BY (SELECT 3) LIMIT (SELECT 4) OFFSET (SELECT 5)",
        "(SELECT (SELECT 1) INTERSECT SELECT (SELECT 2)) EXCEPT ALL (SELECT (SELECT 3) UNION SELECT (SELECT 4))",
        "SELECT NOT ((SELECT 1) = 2), (SELECT NOT ((SELECT 3) = 4)), (SELECT 5)",
    ].into_iter().map(statement).collect::<Vec<_>>();
    sources.push(Statement::Values { rows: Vec::new() });
    sources.push(Statement::Values {
        rows: vec![
            vec![],
            vec![Expr::ScalarSubquery(Box::new(blocks::query("SELECT 8")))],
        ],
    });
    let mut implicit = blocks::query("SELECT (SELECT 1)");
    implicit.set_op =
        blocks::query("SELECT 1 UNION ALL SELECT (SELECT 2) ORDER BY (SELECT 3)").set_op;
    implicit.set_op.as_mut().unwrap().left = None;
    sources.push(Statement::Select(Box::new(implicit)));
    assert_equivalent(sources);
}

fn assert_equivalent(sources: impl IntoIterator<Item = Statement>) {
    let native = MemoryBudget::new(32 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    for source in sources {
        let source = Arc::new(source);
        for parameters in [false, true] {
            let control = ProductionControl::new(&native, &token, &token);
            let expected = UnifiedPlan::lower_borrowed_with_parameters(
                &source,
                &NoRegisteredAggregates,
                &control,
                parameters.then_some(&replace),
            )
            .unwrap();
            let (query, polls) = complete(&mut input(
                source.clone(),
                &native,
                &work,
                &token,
                &token,
                parameters,
            ));
            assert_eq!(
                json(query.as_plan_ref()),
                json(expected.as_plan_ref()),
                "parameters={parameters}"
            );
            if !parameters {
                let ordinary = UnifiedPlan::lower(source.as_ref().clone());
                assert_eq!(json(query.as_plan_ref()), json(PlanRef::from(&ordinary)));
            }
            assert!(polls > 1);
            assert_eq!(allocation_counter::measure(|| drop(query)).count_total, 0);
            drop(expected);
            assert_eq!((native.used(), work.used()), (0, 0));
        }
    }
}

#[test]
fn retained_enclosing_queries_release_each_pending_arena_and_set_prefix() {
    assert_prefix_retirement(statement("SELECT (SELECT 1), (SELECT 2) IN (SELECT 3) UNION ALL SELECT (SELECT 4) ORDER BY (SELECT 5) LIMIT (SELECT 6)"));
}

fn assert_prefix_retirement(source: Statement) {
    let source = Arc::new(source);
    let native = MemoryBudget::new(32 << 20);
    let work = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let (output, polls) = complete(&mut input(
        source.clone(),
        &native,
        &work,
        &token,
        &token,
        false,
    ));
    drop(output);
    let sibling = native.reserve(37).unwrap();
    for prefix in 0..polls - 1 {
        for cancel in 0..3 {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let mut reader = input(source.clone(), &native, &work, &original, &invoking, false);
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
                    advance(&mut reader).err().unwrap().sqlstate(),
                    Some("57014")
                );
            }
            drop(reader);
            assert_eq!(
                (native.used(), work.used()),
                (37, 0),
                "prefix={prefix}, cancellation={cancel}"
            );
        }
    }
    drop(sibling);
    for bytes in [0, 1, 512, 4096, 8192, 32768] {
        let denied = MemoryBudget::new(bytes);
        let error = input(source.clone(), &denied, &work, &token, &token, false)
            .read(&mut std::io::empty())
            .err()
            .unwrap();
        assert_eq!(error.sqlstate(), Some("53200"));
        assert_eq!((denied.used(), work.used()), (0, 0));
        assert!(denied.peak() <= bytes);
    }
}

#[test]
fn retained_enclosing_queries_bound_nested_polling_and_retire_late_failures() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let native = MemoryBudget::new(64 << 20);
            let work = MemoryBudget::new(1 << 20);
            let token = CancellationToken::new();
            for fail in [false, true] {
                let mut source = blocks::query("SELECT 1");
                for _ in 0..512 {
                    let mut parent = blocks::query("SELECT 1");
                    parent.projections[0].expr = Expr::ScalarSubquery(Box::new(source));
                    source = parent;
                }
                if fail {
                    source.projections.push(
                        blocks::query("SELECT reject_function()")
                            .projections
                            .remove(0),
                    );
                }
                let result = input(
                    Arc::new(Statement::Select(Box::new(source))),
                    &native,
                    &work,
                    &token,
                    &token,
                    false,
                )
                .read(&mut std::io::empty());
                if fail {
                    assert_eq!(
                        result.err().unwrap().to_string(),
                        "internal error: rejected aggregate classification"
                    );
                } else {
                    let query = result.unwrap();
                    assert_eq!(allocation_counter::measure(|| drop(query)).count_total, 0);
                }
                assert_eq!((native.used(), work.used()), (0, 0));
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
