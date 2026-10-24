//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

#[derive(Default)]
struct Counts {
    next: AtomicUsize,
    close: AtomicUsize,
    drop: AtomicUsize,
}

struct Source {
    counts: Arc<Counts>,
    remaining: usize,
    next_error: bool,
    close_error: bool,
}

fn error(state: &str, message: &str) -> SQLError {
    SQLError::Diagnostic {
        sqlstate: state.into(),
        message: message.into(),
        detail: Some(format!("{message} detail")),
        hint: Some(format!("{message} hint")),
    }
}

impl SQLRowSource<usize> for Source {
    fn next_row(&mut self) -> Result<Option<usize>, SQLError> {
        self.counts.next.fetch_add(1, Ordering::SeqCst);
        if self.remaining > 0 {
            self.remaining -= 1;
            return Ok(Some(self.remaining));
        }
        if self.next_error {
            Err(error("22012", "read failure"))
        } else {
            Ok(None)
        }
    }

    fn close(&mut self) -> Result<(), SQLError> {
        assert_eq!(self.counts.close.fetch_add(1, Ordering::SeqCst), 0);
        if self.close_error {
            Err(error("58030", "close failure"))
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

fn stream(
    remaining: usize,
    next_error: bool,
    close_error: bool,
) -> (SQLRowStream<'static, usize>, Arc<Counts>) {
    let counts = Arc::new(Counts::default());
    (
        SQLRowStream::from_source(Source {
            counts: counts.clone(),
            remaining,
            next_error,
            close_error,
        }),
        counts,
    )
}

#[test]
fn exhaustion_and_failure_close_once_and_fuse_even_when_cleanup_fails() {
    for next_error in [false, true] {
        for close_error in [false, true] {
            let (mut rows, counts) = stream(1, next_error, close_error);
            assert_eq!(rows.next().unwrap().unwrap(), 0);
            let final_row = rows.next();
            match (next_error, close_error) {
                (false, false) => assert!(final_row.is_none()),
                (false, true) => {
                    let error = final_row.unwrap().unwrap_err();
                    assert_eq!(error.sqlstate(), Some("58030"));
                    assert_eq!(error.detail(), Some("close failure detail"));
                }
                (true, cleanup) => {
                    let error = final_row.unwrap().unwrap_err();
                    assert_eq!(error.sqlstate(), Some("22012"));
                    assert_eq!(error.to_string(), "read failure");
                    assert_eq!(error.hint(), Some("read failure hint"));
                    assert!(error.detail().unwrap().starts_with("read failure detail"));
                    assert_eq!(error.cleanup().is_some(), cleanup);
                    if cleanup {
                        assert_eq!(error.cleanup().unwrap().1.sqlstate(), Some("58030"));
                    }
                }
            }
            assert!(rows.next().is_none());
            rows.close().unwrap();
            drop(rows);
            assert_eq!(counts.next.load(Ordering::SeqCst), 2);
            assert_eq!(counts.close.load(Ordering::SeqCst), 1);
            assert_eq!(counts.drop.load(Ordering::SeqCst), 1);
        }
    }
}

#[test]
fn early_close_and_abandonment_do_not_read_more_rows_or_repeat_cleanup() {
    for close_error in [false, true] {
        let (mut rows, counts) = stream(100, false, close_error);
        assert_eq!(rows.next().unwrap().unwrap(), 99);
        assert_eq!(rows.close().is_err(), close_error);
        rows.close().unwrap();
        assert!(rows.next().is_none());
        drop(rows);
        assert_eq!(counts.next.load(Ordering::SeqCst), 1);
        assert_eq!(counts.close.load(Ordering::SeqCst), 1);
        assert_eq!(counts.drop.load(Ordering::SeqCst), 1);

        let (rows, counts) = stream(100, false, close_error);
        drop(rows);
        assert_eq!(counts.next.load(Ordering::SeqCst), 0);
        assert_eq!(counts.close.load(Ordering::SeqCst), 1);
        assert_eq!(counts.drop.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn mapping_keeps_early_cleanup_and_combines_projection_and_close_failures() {
    let (rows, counts) = stream(100, false, true);
    let mut rows = rows.map_rows(|_| Err::<usize, _>(error("22003", "projection failure")));
    let error = rows.next().unwrap().unwrap_err();
    assert_eq!(error.sqlstate(), Some("22003"));
    assert_eq!(error.to_string(), "projection failure");
    assert_eq!(error.cleanup().unwrap().1.sqlstate(), Some("58030"));
    assert!(rows.next().is_none());
    rows.close().unwrap();
    assert_eq!(counts.next.load(Ordering::SeqCst), 1);
    assert_eq!(counts.close.load(Ordering::SeqCst), 1);

    let (rows, counts) = stream(100, false, true);
    let mut rows = rows
        .map_rows(|row| Ok(row + 1))
        .map_rows(|row| Ok(row.to_string()));
    assert_eq!(rows.next().unwrap().unwrap(), "100");
    assert_eq!(rows.close().unwrap_err().sqlstate(), Some("58030"));
    assert_eq!(counts.next.load(Ordering::SeqCst), 1);
    assert_eq!(counts.close.load(Ordering::SeqCst), 1);
}

#[test]
fn physical_scan_reports_early_close_failure() {
    use crate::{PhysicalOperator, PhysicalRow, PhysicalRowIteratorScan, RowSchema};
    let (rows, counts) = stream(100, false, true);
    let rows = rows.map_rows(|row| {
        Ok(PhysicalRow::from_values(vec![uqa_core::Value::Int(
            row as i64,
        )]))
    });
    let mut scan = PhysicalRowIteratorScan::from_stream(RowSchema::new(vec!["n".into()]), rows);
    scan.open().unwrap();
    let batch = scan
        .next_with_demand(std::num::NonZeroUsize::new(1).unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(batch.len(), 1);
    let error = crate::physical::physical_exec_error(scan.close().unwrap_err());
    assert_eq!(error.sqlstate(), Some("58030"));
    scan.close().unwrap();
    assert!(scan.next().unwrap().is_none());
    assert_eq!(counts.next.load(Ordering::SeqCst), 1);
    assert_eq!(counts.close.load(Ordering::SeqCst), 1);
}
