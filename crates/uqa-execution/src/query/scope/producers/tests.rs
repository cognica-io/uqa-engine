//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{PhysicalRow, PhysicalRowIteratorScan, RowSchema, SQLRowSource, SQLRowStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use uqa_core::Value;

#[derive(Default)]
struct Counts {
    read: AtomicUsize,
    close: AtomicUsize,
    drop: AtomicUsize,
}

struct Source {
    counts: Arc<Counts>,
    scope: CteScope,
    label: &'static str,
    fail: bool,
    order: Arc<Mutex<Vec<&'static str>>>,
}

impl SQLRowSource<PhysicalRow> for Source {
    fn next_row(&mut self) -> Result<Option<PhysicalRow>, SQLError> {
        assert_eq!(self.counts.close.load(Ordering::SeqCst), 0);
        let n = self.counts.read.fetch_add(1, Ordering::SeqCst);
        Ok(Some(PhysicalRow::from_values(vec![Value::Int(
            i64::try_from(n).unwrap(),
        )])))
    }
    fn close(&mut self) -> Result<(), SQLError> {
        assert_eq!(self.counts.close.fetch_add(1, Ordering::SeqCst), 0);
        self.order.lock().push(self.label);
        // A real retained input holds its lexical scope through close and drop.
        let _ = &self.scope;
        if self.fail {
            Err(SQLError::Diagnostic {
                sqlstate: "58030".into(),
                message: self.label.into(),
                detail: Some(format!("{} detail", self.label)),
                hint: Some(format!("{} hint", self.label)),
            })
        } else {
            Ok(())
        }
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        assert_eq!(self.counts.close.load(Ordering::SeqCst), 1);
        self.counts.drop.fetch_add(1, Ordering::SeqCst);
    }
}

fn input(
    scope: &CteScope,
    label: &'static str,
    fail: bool,
    order: &Arc<Mutex<Vec<&'static str>>>,
) -> (Box<dyn PhysicalOperator>, Arc<Counts>) {
    let counts = Arc::new(Counts::default());
    let input = PhysicalRowIteratorScan::from_stream(
        RowSchema::new(vec!["n".into()]),
        SQLRowStream::from_source(Source {
            counts: counts.clone(),
            scope: scope.clone(),
            label,
            fail,
            order: order.clone(),
        }),
    );
    (Box::new(input), counts)
}

fn add(scope: &mut CteScope, label: &'static str, fail: bool) -> Arc<Counts> {
    let (input, counts) = input(scope, label, fail, &Arc::default());
    scope.insert_producer(label.into(), input, 1).unwrap();
    counts
}

fn reader(scope: &CteScope, name: &str) -> Box<dyn PhysicalOperator> {
    scope.rows.get(name).unwrap().clone().into_operator()
}

fn read(input: &mut dyn PhysicalOperator) -> Value {
    let batch = input
        .next_with_demand(crate::physical::demand::ONE)
        .unwrap()
        .unwrap();
    batch.into_owned_rows()[0]
        .view()
        .value_at(0)
        .unwrap()
        .clone()
}

fn closed(counts: &Counts, reads: usize) {
    assert_eq!(counts.read.load(Ordering::SeqCst), reads);
    assert_eq!(counts.close.load(Ordering::SeqCst), 1);
    assert_eq!(counts.drop.load(Ordering::SeqCst), 1);
}

#[test]
fn statement_owner_closes_unopened_and_partial_producers_with_live_readers() {
    for fail in [false, true] {
        for rows in [0, 2] {
            let mut scope = CteScope::default();
            let mut counts = None;
            let mut retained_reader = None;
            let result = scope.with_cte_producers(true, |scope| {
                counts = Some(add(scope, "local", fail));
                let mut first = reader(scope, "local");
                let mut second = reader(scope, "local");
                if rows != 0 {
                    first.open().unwrap();
                    assert_eq!(read(first.as_mut()), Value::Int(0));
                    first.close().unwrap();
                    second.open().unwrap();
                    assert_eq!(read(second.as_mut()), Value::Int(0));
                    assert_eq!(read(second.as_mut()), Value::Int(1));
                }
                assert_eq!(counts.as_ref().unwrap().close.load(Ordering::SeqCst), 0);
                retained_reader = Some(second);
                Ok(())
            });
            if fail {
                let error = result.unwrap_err();
                assert_eq!(error.sqlstate(), Some("58030"));
                assert_eq!(error.hint(), Some("local hint"));
            } else {
                result.unwrap();
            }
            closed(counts.as_ref().unwrap(), rows);
            drop(retained_reader);
            drop(scope);
            closed(counts.as_ref().unwrap(), rows);
        }
    }
}

#[test]
fn child_query_finalization_does_not_end_inherited_producers() {
    let mut parent = CteScope::default();
    let inherited = add(&mut parent, "parent", false);
    let mut local = None;
    parent
        .with_cte_producers(true, |scope| {
            local = Some(add(scope, "child", false));
            let mut input = reader(scope, "parent");
            input.open().unwrap();
            assert_eq!(read(input.as_mut()), Value::Int(0));
            input.close().unwrap();
            Ok(())
        })
        .unwrap();
    closed(local.as_ref().unwrap(), 0);
    assert_eq!(inherited.close.load(Ordering::SeqCst), 0);
    let mut input = reader(&parent, "parent");
    input.open().unwrap();
    assert_eq!(read(input.as_mut()), Value::Int(0));
    assert_eq!(read(input.as_mut()), Value::Int(1));
    parent.finish_cte_producers(Ok(())).unwrap();
    closed(&inherited, 2);
}

#[test]
fn all_producers_close_in_reverse_creation_order_and_keep_every_error() {
    let mut scope = CteScope::default();
    let order = Arc::default();
    let mut counts = Vec::new();
    for label in ["first", "second", "third"] {
        let (input, count) = input(&scope, label, true, &order);
        scope.insert_producer(label.into(), input, 1).unwrap();
        counts.push(count);
    }
    let error = scope
        .finish_cte_producers::<()>(Err(SQLError::Routine {
            sqlstate: "22012".into(),
            message: "primary failure".into(),
        }))
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("22012"));
    assert_eq!(error.to_string(), "primary failure");
    for label in ["first", "second", "third"] {
        assert!(error.detail().unwrap().contains(&format!("{label} hint")));
    }
    assert_eq!(*order.lock(), ["third", "second", "first"]);
    scope.finish_cte_producers(Ok(())).unwrap();
    drop(scope);
    for count in counts {
        closed(&count, 0);
    }
}

#[test]
fn retained_query_transfers_local_ownership_and_closes_before_or_after_open() {
    for open in [false, true] {
        let mut scope = CteScope::default();
        let mut counts = None;
        let mut query = scope
            .retain_cte_producers(true, |scope| {
                counts = Some(add(scope, "retained", true));
                Ok(Some(reader(scope, "retained")))
            })
            .unwrap()
            .unwrap();
        drop(scope);
        assert_eq!(counts.as_ref().unwrap().close.load(Ordering::SeqCst), 0);
        if open {
            query.open().unwrap();
            assert_eq!(read(query.as_mut()), Value::Int(0));
        }
        let error = physical_exec_error(query.close().unwrap_err());
        assert_eq!(error.sqlstate(), Some("58030"));
        query.close().unwrap();
        drop(query);
        closed(counts.as_ref().unwrap(), usize::from(open));
    }
}

#[test]
fn retained_close_reports_input_and_producer_failures_and_releases_both() {
    let mut scope = CteScope::default();
    let order = Arc::default();
    let mut counts = Vec::new();
    let mut query = scope
        .retain_cte_producers(true, |scope| {
            let (producer, count) = input(scope, "producer", true, &order);
            scope.insert_producer("producer".into(), producer, 1)?;
            counts.push(count);
            let (pipeline, count) = input(scope, "pipeline", true, &order);
            counts.push(count);
            Ok(Some(pipeline))
        })
        .unwrap()
        .unwrap();
    let error = physical_exec_error(query.close().unwrap_err());
    assert_eq!(error.to_string(), "pipeline");
    assert!(error.detail().unwrap().contains("producer hint"));
    assert_eq!(*order.lock(), ["pipeline", "producer"]);
    drop(query);
    drop(scope);
    for count in counts {
        closed(&count, 0);
    }
}

#[test]
fn abandoned_retained_construction_reports_producer_close_failure() {
    for fail_build in [false, true] {
        let mut scope = CteScope::default();
        let mut counts = None;
        let result = scope.retain_cte_producers(true, |scope| {
            counts = Some(add(scope, "abandoned", true));
            if fail_build {
                Err(SQLError::Routine {
                    sqlstate: "22012".into(),
                    message: "primary".into(),
                })
            } else {
                Ok(None)
            }
        });
        let Err(error) = result else {
            panic!("cleanup failure was lost")
        };
        assert_eq!(
            error.sqlstate(),
            Some(if fail_build { "22012" } else { "58030" })
        );
        closed(counts.as_ref().unwrap(), 0);
    }
}

#[test]
fn abandoned_owner_releases_producers_despite_retained_lexical_scopes() {
    let mut scope = CteScope::default();
    let counts = add(&mut scope, "abandoned", false);
    let borrowed = scope.clone();
    drop(scope);
    closed(&counts, 0);
    drop(borrowed);
    closed(&counts, 0);
}

#[test]
fn retired_or_expired_owner_closes_rejected_input_without_losing_its_error() {
    for expired in [false, true] {
        let scope = CteScope::default();
        let mut borrowed = scope.clone();
        scope.finish_cte_producers(Ok(())).unwrap();
        let scope = (!expired).then_some(scope);
        let (input, counts) = input(&borrowed, "rejected", true, &Arc::default());
        let error = borrowed
            .insert_producer("late".into(), input, 1)
            .unwrap_err();
        assert!(error.detail().unwrap().contains("rejected hint"));
        closed(&counts, 0);
        drop(scope);
    }
}

mod production;
