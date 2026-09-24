//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Unconsumed catalog/retrieval sources preserve laziness and typed execution errors.

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn deferred_sources_run_once_on_pull_and_remain_unused_when_closed_without_rows() {
    for pull in [false, true] {
        let calls = AtomicUsize::new(0);
        let mut scan = DeferredTableScan::new(
            RowSchema::default(),
            Box::new(|| {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(Box::new(crate::scan::VecSource::with_row_schema(
                    RowSchema::default(),
                    vec![],
                )))
            }),
        );
        scan.open().unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        if pull {
            assert!(scan.next().unwrap().is_none());
            assert!(scan.next().unwrap().is_none());
        }
        scan.close().unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), usize::from(pull));
    }
}

#[test]
fn deferred_source_failures_keep_sqlstate_and_never_repeat_the_producer() {
    let calls = AtomicUsize::new(0);
    let mut scan = DeferredTableScan::new(
        RowSchema::default(),
        Box::new(|| {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(SQLError::Routine {
                sqlstate: "40001".into(),
                message: "retained participant completed".into(),
            })
        }),
    );
    scan.open().unwrap();
    let error = scan.next().unwrap_err();
    assert!(
        matches!(error, crate::ExecError::SQL(SQLError::Routine { ref sqlstate, .. }) if sqlstate == "40001")
    );
    assert!(scan.next().is_err());
    scan.close().unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
