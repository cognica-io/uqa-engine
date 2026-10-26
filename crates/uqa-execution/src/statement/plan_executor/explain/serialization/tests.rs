//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::query::{diagnostics::QueryDiagnostics, measurement::QueryMeasurement};
use std::{
    io::{Read, Seek, Write},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Mutex,
    },
};
use uqa_core::{io_usage::IOOptions, Value};
use uqa_sql::{expr::EngineHook, ColumnType};
use uqa_storage::{read_control::StorageReadControl, temporary_file::BlockTemporaryFile};

struct Catalog {
    file: Mutex<BlockTemporaryFile<4>>,
    calls: AtomicUsize,
    panic: AtomicBool,
}

impl Catalog {
    fn new() -> Self {
        let mut file = BlockTemporaryFile::new().unwrap();
        for _ in 0..8 {
            file.write_all(b"name").unwrap();
        }
        file.rewind().unwrap();
        Self {
            file: Mutex::new(file),
            calls: AtomicUsize::new(0),
            panic: AtomicBool::new(false),
        }
    }
}

impl EngineHook for Catalog {
    fn nextval(&self, _: &str) -> Result<i64, SQLError> {
        panic!("unexpected sequence call")
    }
    fn currval(&self, _: &str) -> Result<i64, SQLError> {
        panic!("unexpected sequence call")
    }
    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64, SQLError> {
        panic!("unexpected sequence call")
    }
    fn resolve_regtype_output(&self, ty: &ColumnType, oid: i64) -> Result<Option<String>, String> {
        assert_eq!(ty, &ColumnType::Regclass);
        assert_eq!(oid, 42);
        let mut bytes = [0; 4];
        self.file.lock().unwrap().read_exact(&mut bytes).unwrap();
        self.calls.fetch_add(1, Ordering::Relaxed);
        assert!(
            !self.panic.load(Ordering::Relaxed),
            "output catalog panic after reading"
        );
        Ok(Some(String::from_utf8(bytes.to_vec()).unwrap()))
    }
}

#[test]
fn conversion_io_is_separate_from_plan_output_and_admitted_before_catalog_reads() {
    let catalog = Arc::new(Catalog::new());
    let retained = StorageReadControl::with_limit(65_536);
    let invoking = uqa_core::CancellationToken::new();
    let owner = QueryDiagnostics::default();
    let scope = owner
        .enter_measured(
            retained.clone(),
            false,
            IOOptions {
                buffers: true,
                wal: false,
            },
        )
        .unwrap();
    let serializer = RowSerialization::new(
        ExplainSerialization::Text,
        false,
        catalog.clone(),
        &retained,
        &invoking,
    )
    .unwrap();
    let schema = RowSchema::with_types(vec!["value".into()], vec![Some(ColumnType::Regclass)]);
    let row = crate::PhysicalRow::from_values(vec![Value::Int(42)]);
    serializer.begin(schema.columns(), &schema).unwrap();
    assert_eq!(catalog.calls.load(Ordering::Relaxed), 0);
    let measurement = QueryMeasurement::new(false, retained.memory());
    measurement
        .run(|| {
            for _ in 0..3 {
                measurement.output(1, || serializer.consume(&schema, &row))?;
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(measurement.io(), IOUsage::default());
    assert_eq!(serializer.metrics().io.temporary.reads, 3);
    assert_eq!(serializer.metrics().bytes, 30);
    assert!(serializer.metrics().elapsed.is_zero());
    let baseline = retained.memory().used();
    let reservation = retained.memory().reserve(65_536 - baseline).unwrap();
    assert_eq!(
        serializer.consume(&schema, &row).unwrap_err().sqlstate(),
        Some("53200")
    );
    assert_eq!(catalog.calls.load(Ordering::Relaxed), 3);
    drop(reservation);
    for token in [retained.cancellation(), &invoking] {
        token.cancel();
        assert_eq!(
            serializer.consume(&schema, &row).unwrap_err().sqlstate(),
            Some("57014")
        );
        assert_eq!(catalog.calls.load(Ordering::Relaxed), 3);
        token.reset();
    }
    catalog.panic.store(true, Ordering::Relaxed);
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(
        || serializer.consume(&schema, &row)
    ))
    .is_err());
    catalog.panic.store(false, Ordering::Relaxed);
    assert_eq!(retained.memory().used(), baseline);
    assert_eq!(serializer.metrics().io.temporary.reads, 3);
    serializer.consume(&schema, &row).unwrap();
    assert_eq!(serializer.metrics().io.temporary.reads, 4);
    let mut last = serializer.metrics();
    last.io.temporary.reads = u64::MAX;
    serializer.metrics.set(last);
    assert!(serializer.consume(&schema, &row).is_err());
    assert_eq!(serializer.metrics().bytes, last.bytes);
    assert_eq!(serializer.metrics().io, last.io);
    assert_eq!(retained.memory().used(), baseline);
    drop((serializer, measurement));
    let captured = scope.finish_execution().unwrap();
    assert_eq!(captured.io.temporary.reads, 6);
    drop(captured);
    assert_eq!(retained.memory().used(), 0);
}
