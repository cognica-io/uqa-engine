//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{ExecResult, PhysicalRow, RowSchema};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use uqa_core::Value;
use uqa_sql::ast::BinaryOp;

struct WideSource {
    read: Arc<AtomicUsize>,
    closed: Arc<AtomicBool>,
}

impl Drop for WideSource {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Relaxed);
    }
}

impl Iterator for WideSource {
    type Item = ExecResult<PhysicalRow>;
    fn next(&mut self) -> Option<Self::Item> {
        let id = self.read.fetch_add(1, Ordering::Relaxed);
        if id == 256 {
            return None;
        }
        let id = if id == 255 { u64::MAX } else { id as u64 };
        Some(Ok(PhysicalRow::from_values(vec![Value::Str(
            "x".repeat(64 * 1024),
        )])
        .with_lock_origin(crate::RowLockOrigin::new("t", "t", id))))
    }
}

fn source(predicate: ScalarExpr) -> (Filter<'static>, Arc<AtomicUsize>, Arc<AtomicBool>) {
    let read = Arc::new(AtomicUsize::new(0));
    let closed = Arc::new(AtomicBool::new(false));
    let scan = PhysicalRowIteratorScan::new(
        RowSchema::new(vec!["body".into()]),
        Box::new(WideSource {
            read: Arc::clone(&read),
            closed: Arc::clone(&closed),
        }),
    )
    .with_batch_size(std::num::NonZeroUsize::MIN);
    (Filter::new(Box::new(scan), predicate, vec![]), read, closed)
}

#[test]
fn scalar_where_visits_wide_rows_without_retaining_filter_payloads() {
    let (mut filter, read, closed) = source(ScalarExpr::Literal(Value::Bool(true)));
    let mut delivered = 0;
    let allocated = allocation_counter::measure(|| {
        visit_filtered(&mut filter, &mut |id| {
            assert_eq!(
                id,
                if delivered == 255 {
                    u64::MAX
                } else {
                    delivered
                }
            );
            delivered += 1;
            Ok(())
        })
        .unwrap();
    });
    assert_eq!(delivered, 256);
    assert_eq!(read.load(Ordering::Relaxed), 257);
    drop(filter);
    assert!(closed.load(Ordering::Relaxed));
    // Sixteen MiB of source values pass through one live row and fixed filter state.
    assert!(allocated.bytes_max < 512 * 1024, "{allocated:?}");
}

#[test]
fn scalar_where_closes_on_consumer_and_predicate_errors() {
    let (mut filter, read, closed) = source(ScalarExpr::Literal(Value::Bool(true)));
    let failure = visit_filtered(&mut filter, &mut |id| {
        if id == 7 {
            Err(SQLError::Routine {
                sqlstate: "53200".into(),
                message: "consumer allowance exhausted".into(),
            })
        } else {
            Ok(())
        }
    })
    .unwrap_err();
    assert_eq!(failure.sqlstate(), Some("53200"));
    assert_eq!(read.load(Ordering::Relaxed), 8);
    drop(filter);
    assert!(closed.load(Ordering::Relaxed));

    let (mut filter, read, closed) = source(ScalarExpr::Binary {
        op: BinaryOp::Divide,
        lhs: Box::new(ScalarExpr::Literal(Value::Int(1))),
        rhs: Box::new(ScalarExpr::Literal(Value::Int(0))),
    });
    let failure = visit_filtered(&mut filter, &mut |_| {
        panic!("failed predicate cannot deliver an identity")
    })
    .unwrap_err();
    assert_eq!(failure.sqlstate(), Some("22012"));
    assert_eq!(read.load(Ordering::Relaxed), 1);
    drop(filter);
    assert!(closed.load(Ordering::Relaxed));
}
