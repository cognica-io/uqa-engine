//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::aggregation::ordering::AggregateSortKey;
use crate::query::relational::values::test_support::Services;
use std::sync::atomic::{AtomicUsize, Ordering};
use uqa_sql::plan::{RelationalPlan, UnifiedPlan};

#[derive(Default)]
struct Counts {
    created: AtomicUsize,
    closed: AtomicUsize,
    dropped: AtomicUsize,
}

struct Factory {
    counts: Arc<Counts>,
    fail: &'static str,
}

fn failure(stage: &str) -> SQLError {
    SQLError::Diagnostic {
        sqlstate: if stage == "close" { "58000" } else { "P0001" }.into(),
        message: format!("aggregate {stage} failure"),
        detail: Some(format!("{stage} detail")),
        hint: Some(format!("{stage} hint")),
    }
}

impl SQLAggregateFactory for Factory {
    fn try_create_state(&self) -> Result<Box<dyn SQLAggregateState>, SQLError> {
        let number = self.counts.created.fetch_add(1, Ordering::SeqCst) + 1;
        if self.fail == "create" && number == 3 {
            return Err(failure("create"));
        }
        Ok(Box::new(State {
            counts: Arc::clone(&self.counts),
            fail: self.fail,
        }))
    }
}

struct State {
    counts: Arc<Counts>,
    fail: &'static str,
}

impl SQLAggregateState for State {
    fn observe(&mut self, _: &[Value]) -> Result<(), SQLError> {
        if self.fail == "observe" {
            Err(failure("observe"))
        } else {
            Ok(())
        }
    }

    fn finish(&self) -> Result<Value, SQLError> {
        if self.fail == "finish" {
            Err(failure("finish"))
        } else {
            Ok(Value::Int(7))
        }
    }

    fn close(&mut self) -> Result<(), SQLError> {
        self.counts.closed.fetch_add(1, Ordering::SeqCst);
        Err(failure("close"))
    }
}

impl Drop for State {
    fn drop(&mut self) {
        self.counts.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

fn template(fail: &'static str) -> (Arc<Counts>, AggregateAccumulatorTemplate) {
    let counts = Arc::new(Counts::default());
    let template = AggregateAccumulatorTemplate::registered(Arc::new(Factory {
        counts: Arc::clone(&counts),
        fail,
    }));
    (counts, template)
}

fn assert_closed(counts: &Counts, expected: usize) {
    assert_eq!(counts.closed.load(Ordering::SeqCst), expected);
    assert_eq!(counts.dropped.load(Ordering::SeqCst), expected);
}

#[test]
fn failed_close_consumes_state_and_abandoned_state_closes_once() {
    let (counts, template) = template("");
    let mut accumulator = template.instantiate(1024).unwrap();
    assert_eq!(
        aggregate_value("host", &accumulator, None).unwrap(),
        Value::Int(7)
    );
    assert_eq!(accumulator.close().unwrap_err().hint(), Some("close hint"));
    accumulator.close().unwrap();
    assert!(aggregate_value("host", &accumulator, None).is_err());
    drop(accumulator);
    assert_closed(&counts, 1);
    drop(template.instantiate(1024).unwrap());
    assert_closed(&counts, 2);
}

#[test]
fn partial_creation_preserves_primary_and_all_cleanup_failures() {
    let (counts, template) = template("create");
    let error = match instantiate_aggregate_accumulators(&vec![template; 4], 1024) {
        Ok(_) => panic!("creation should fail"),
        Err(error) => error,
    };
    assert_eq!(counts.created.load(Ordering::SeqCst), 3);
    assert_closed(&counts, 2);
    assert_eq!(error.sqlstate(), Some("P0001"));
    assert_eq!(error.hint(), Some("create hint"));
    assert_eq!(
        error
            .detail()
            .unwrap()
            .matches("aggregate close failure")
            .count(),
        2
    );
    assert_eq!(error.cleanup().unwrap().1.sqlstate(), Some("58000"));
}

#[test]
fn ordered_and_distinct_replay_close_before_returning_values_or_errors() {
    for distinct in [false, true] {
        for budget in [1, 1 << 20] {
            for fail in ["", "observe", "finish"] {
                let (counts, template) = template(fail);
                let mut accumulator = template.instantiate(budget).unwrap();
                for value in [3, 1, 2, 1] {
                    let args = vec![Value::Int(value)];
                    if distinct {
                        accumulator
                            .distinct
                            .insert(&Value::List(args), Vec::new(), None)
                            .unwrap();
                    } else {
                        accumulator
                            .observe_registered(
                                args,
                                vec![AggregateSortKey::ascending(Value::Int(value))],
                                None,
                            )
                            .unwrap();
                    }
                }
                let error = aggregate_value("host", &accumulator, None).unwrap_err();
                assert_closed(&counts, 1);
                assert_eq!(counts.created.load(Ordering::SeqCst), 2);
                if fail.is_empty() {
                    assert_eq!(error.sqlstate(), Some("58000"));
                    assert_eq!(error.detail(), Some("close detail"));
                } else {
                    assert_eq!(error.sqlstate(), Some("P0001"));
                    assert_eq!(error.hint(), Some(format!("{fail} hint")).as_deref());
                    assert_eq!(error.cleanup().unwrap().1.hint(), Some("close hint"));
                }
                accumulator.close().unwrap_err();
                assert!(aggregate_value("host", &accumulator, None).is_err());
                assert_eq!(counts.created.load(Ordering::SeqCst), 2);
                drop(accumulator);
                assert_closed(&counts, 2);
            }
        }
    }
}

#[test]
fn group_output_closes_every_state_on_success_filter_or_expression_failure() {
    for (sql, primary) in [
        ("SELECT count(*)", "58000"),
        ("SELECT count(*) HAVING count(*) < 0", "58000"),
        ("SELECT count(*) / (count(*) - 7)", "22012"),
    ] {
        let UnifiedPlan::Query(query) =
            UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
        else {
            panic!("query")
        };
        let RelationalPlan::QueryBlock(block) = query.root else {
            panic!("block")
        };
        let targets: Vec<_> = aggregate_targets(&Services, &block)
            .into_iter()
            .cloned()
            .collect();
        let plan = super::super::output::AggregateOutputPlan::compile(
            &Services,
            &block,
            &targets,
            false,
            &crate::RowSchema::default(),
            &[],
        )
        .unwrap();
        let (counts, template) = template("");
        let accumulators =
            instantiate_aggregate_accumulators(&vec![template; targets.len()], 1024).unwrap();
        let error = super::super::output::finish_group(
            &Services,
            &block,
            &plan,
            accumulators,
            &[],
            &["v".into()],
            &[],
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some(primary), "{sql}: {error}");
        assert_closed(&counts, targets.len());
    }
}

#[test]
fn aggregate_executor_rejects_consumption_and_finalization_after_close() {
    use crate::AggregateExecutor;
    let UnifiedPlan::Query(query) =
        UnifiedPlan::lower(uqa_sql::compile("SELECT count(*)").unwrap().remove(0))
    else {
        panic!("query")
    };
    let RelationalPlan::QueryBlock(block) = query.root else {
        panic!("block")
    };
    let mut executor = PhysicalAggregateExecutor::new(
        Arc::new(Services),
        &block,
        &[],
        crate::RowSchema::default(),
        crate::RowSchema::new(vec!["v".into()]),
        1024,
    )
    .unwrap();
    executor.close().unwrap();
    executor.close().unwrap();
    assert!(!executor.supports_projected_rows());
    assert!(!executor.supports_storage_borrowed_rows());
    assert!(executor
        .consume(crate::Batch::from_physical_rows(
            crate::RowSchema::default(),
            Vec::new()
        ))
        .is_err());
    assert!(executor.finish().is_err());
}
