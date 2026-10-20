//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{Batch, PhysicalOperator, PhysicalRow, RowSchema, SpillBuffer};
use uqa_core::Value;
use uqa_sql::ColumnType;

mod admitted;

fn rows(count: usize, payload: usize) -> (InternalRelationId, SharedSpill) {
    let relation = InternalRelationId::allocate();
    let schema = RowSchema::with_internal_relation_metadata(
        relation,
        [
            (Some(ColumnType::BigInteger), None),
            (Some(ColumnType::Text), Some(950)),
        ],
    );
    let mut spill = SpillBuffer::new(4096);
    for index in 0..count {
        spill
            .push(Batch::from_physical_rows(
                schema.clone(),
                vec![PhysicalRow::from_values(vec![
                    Value::Int(index as i64),
                    Value::Str("x".repeat(payload)),
                ])],
            ))
            .unwrap();
    }
    (relation, spill.into_shared(schema).unwrap())
}

#[test]
fn retained_rows_stream_repeatable_spilled_inputs_with_bounded_memory() {
    let allocated = allocation_counter::measure(|| {
        let (relation, rows) = rows(256, 64 * 1024);
        assert!(rows.has_spilled());
        let scope = RetainedRowScope::enter(relation, rows).unwrap();
        let mut first = build_retained_source(scope.descriptor()).unwrap();
        let mut second = build_retained_source(scope.descriptor()).unwrap();
        assert!(first.row_schema().shares_index(scope.descriptor().schema()));
        assert_eq!(first.estimated_cardinality(), Some(256));
        assert_eq!(
            first.row_schema().internal_collation(relation.column(1)),
            Some(950)
        );
        first.open().unwrap();
        second.open().unwrap();
        for expected in 0..256 {
            for reader in [&mut first, &mut second] {
                let batch = reader.next().unwrap().unwrap();
                assert_eq!(batch.rows.len(), 1);
                assert_eq!(batch.rows[0].value(0), Some(&Value::Int(expected)));
                let Some(Value::Str(payload)) = batch.rows[0].value(1) else {
                    panic!("payload")
                };
                assert_eq!(payload.len(), 64 * 1024);
                assert!(payload.bytes().all(|byte| byte == b'x'));
            }
        }
        assert!(first.next().unwrap().is_none());
        assert!(second.next().unwrap().is_none());
        first.close().unwrap();
        second.close().unwrap();
        first.open().unwrap();
        assert_eq!(
            first.next().unwrap().unwrap().rows[0].value(0),
            Some(&Value::Int(0))
        );
    });
    assert!(allocated.bytes_max < 2 * 1024 * 1024, "{allocated:?}");
}

#[test]
fn retained_row_scopes_preserve_nested_owners_and_remove_failed_actions() {
    let (outer_id, outer_rows) = rows(1, 32);
    let outer = RetainedRowScope::enter(outer_id, outer_rows.clone()).unwrap();
    assert!(RetainedRowScope::enter(outer_id, outer_rows).is_err());
    let (inner_id, inner_rows) = rows(1, 64);
    let inner_descriptor =
        RetainedRowSource::new(inner_id, inner_rows.row_schema().clone(), 1).unwrap();
    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let inner = RetainedRowScope::enter(inner_id, inner_rows).unwrap();
        assert!(build_retained_source(outer.descriptor()).is_ok());
        assert!(build_retained_source(inner.descriptor()).is_ok());
        panic!("action failure");
    }));
    assert!(unwind.is_err());
    assert!(build_retained_source(&inner_descriptor).is_err());
    assert!(build_retained_source(outer.descriptor()).is_ok());
    let outer_descriptor = outer.descriptor().clone();
    let mut opened = build_retained_source(&outer_descriptor).unwrap();
    opened.open().unwrap();
    drop(outer);
    assert!(build_retained_source(&outer_descriptor).is_err());
    assert_eq!(
        opened.next().unwrap().unwrap().rows[0].value(0),
        Some(&Value::Int(0))
    );
}

#[test]
fn retained_row_sources_reject_mismatched_descriptors_and_keep_empty_inputs() {
    for count in [0, 1] {
        let (relation, rows) = rows(count, 8);
        let scope = RetainedRowScope::enter(relation, rows).unwrap();
        let wrong_count =
            RetainedRowSource::new(relation, scope.descriptor().schema().clone(), count + 1)
                .unwrap();
        assert!(build_retained_source(&wrong_count).is_err());
        let wrong_type = RetainedRowSource::new(
            relation,
            RowSchema::with_internal_relation_types(
                relation,
                vec![Some(ColumnType::Integer), Some(ColumnType::Text)],
            ),
            count,
        )
        .unwrap();
        assert!(build_retained_source(&wrong_type).is_err());
        let mut scan = build_retained_source(scope.descriptor()).unwrap();
        scan.open().unwrap();
        assert_eq!(scan.next().unwrap().is_some(), count != 0);
    }
}

#[test]
fn retained_row_nullary_sources_keep_multiplicity() {
    let relation = InternalRelationId::allocate();
    let schema = RowSchema::default();
    let mut rows = SpillBuffer::new(1);
    rows.push(Batch::from_physical_rows(
        schema.clone(),
        (0..3)
            .map(|_| PhysicalRow::from_values(Vec::new()))
            .collect(),
    ))
    .unwrap();
    let scope = RetainedRowScope::enter(relation, rows.into_shared(schema).unwrap()).unwrap();
    let mut scan = build_retained_source(scope.descriptor()).unwrap();
    assert_eq!(scan.estimated_cardinality(), Some(3));
    assert_eq!(scan.row_schema().physical_width(), 0);
    scan.open().unwrap();
    let mut count = 0;
    while let Some(batch) = scan.next().unwrap() {
        count += batch.rows.len();
    }
    assert_eq!(count, 3);
}

fn read_worker_rows(descriptor: &RetainedRowSource) -> usize {
    let mut scan = build_retained_source(descriptor).unwrap();
    scan.open().unwrap();
    let mut count = 0;
    while let Some(batch) = scan.next().unwrap() {
        for row in batch.rows {
            assert_eq!(row.value(0), Some(&Value::Int(count as i64)));
            let Some(Value::Str(payload)) = row.value(1) else {
                panic!("payload")
            };
            assert_eq!(payload.len(), 64 * 1024);
            assert!(payload.bytes().all(|byte| byte == b'x'));
            count += 1;
        }
    }
    count
}

#[test]
fn retained_row_workers_share_owners_without_row_copies_or_thread_leaks() {
    let (relation, rows) = rows(64, 64 * 1024);
    let scope = RetainedRowScope::enter(relation, rows).unwrap();
    let descriptor = scope.descriptor().clone();
    let allocations = allocation_counter::measure(|| {
        let contexts: Vec<_> = (0..1024).map(|_| RetainedRowContext::current()).collect();
        assert_eq!(contexts.len(), 1024);
    });
    assert!(allocations.bytes_total < 32 * 1024, "{allocations:?}");
    let read = || {
        let allocations =
            allocation_counter::measure(|| assert_eq!(read_worker_rows(&descriptor), 64));
        assert!(allocations.bytes_max < 2 * 1024 * 1024, "{allocations:?}");
    };
    crate::parallel::ParallelExecutor::new(4).execute_branches(&[&read; 4]);
    let counts = crate::parallel::run_parallel(vec![0; 4], 4, |chunk| {
        chunk
            .into_iter()
            .map(|_| read_worker_rows(&descriptor))
            .collect()
    });
    assert_eq!(counts, vec![64; 4]);
    let context = crate::statement::thread_context::StatementThreadContext::current();
    drop(scope);
    assert!(build_retained_source(&descriptor).is_err());
    std::thread::spawn(move || {
        assert!(build_retained_source(&descriptor).is_err());
        {
            let _statement = context.enter();
            assert_eq!(read_worker_rows(&descriptor), 64);
            let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _isolated = RetainedRowContext::default().enter();
                assert!(build_retained_source(&descriptor).is_err());
                panic!("worker failed");
            }));
            assert!(unwind.is_err());
            assert_eq!(read_worker_rows(&descriptor), 64);
        }
        assert!(build_retained_source(&descriptor).is_err());
    })
    .join()
    .unwrap();
}
