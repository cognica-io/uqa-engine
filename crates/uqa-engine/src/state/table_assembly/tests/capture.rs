//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{contents, StorageBackendError, StorageReadControl, TableState};
use crate::state::StorageContext;

fn source_headers(table: &TableState) -> Vec<usize> {
    macro_rules! sizes {
        ($($field:ident),+ $(,)?) => {
            vec![$(table.$field.key().unwrap().retained_source_bytes()),+]
        };
    }
    sizes!(
        columns,
        table_checks,
        hierarchy,
        foreign_keys,
        key_constraints,
        fts_fields,
        row_type_array_name,
        dropped_attributes,
        columns_declared,
        security,
        column_stats,
        analyzer,
        storage_generation,
        next_id,
        maps_integer_keys,
        column_stats_loaded,
        column_stats_dirty,
        statistics_maintenance,
    )
}

#[test]
fn native_table_capture_registration_retains_headers_and_original_generations() {
    let state = StorageContext::memory();
    let control = StorageReadControl::with_limit(8 << 20);
    let table = contents(&control, 1 << 20)
        .into_state(Some(&control))
        .unwrap();
    let before = control.memory().used();
    let columns = table.columns.generation();
    let table = state.table_state(table, Some(&control)).unwrap();
    assert_eq!(
        control.memory().used(),
        before + source_headers(&table).iter().sum::<usize>()
    );
    let occupied = control
        .memory()
        .reserve(control.memory().available())
        .unwrap();
    control.cancellation().cancel();
    let output = StorageReadControl::with_limit(8 << 20);
    let cut = state
        .memory_capture
        .as_ref()
        .unwrap()
        .capture(&output)
        .unwrap();
    let key = table.columns.key().unwrap();
    table.columns.write().publish(Vec::new()).unwrap();
    let saved = key.captured(&cut).unwrap();
    assert!(saved.get().unwrap().shares_definition(&columns));
    drop((columns, table, state, occupied));
    assert!(control.memory().used() > 1 << 20);
    assert_eq!(saved.get().unwrap().len(), 1);
    drop((saved, cut));
    assert_eq!(control.memory().used(), 0);
    assert_eq!(output.memory().used(), 0);
}

#[test]
fn native_table_capture_registration_retires_every_rejected_source_prefix() {
    let state = StorageContext::memory();
    let probe = StorageReadControl::with_limit(1 << 20);
    let table = contents(&probe, 1024).into_state(Some(&probe)).unwrap();
    let table = state.table_state(table, Some(&probe)).unwrap();
    let headers = source_headers(&table);
    drop(table);
    assert_eq!(probe.memory().used(), 0);
    let mut allowed = 0;
    for header in headers {
        let control = StorageReadControl::with_limit(1 << 20);
        let table = contents(&control, 1024).into_state(Some(&control)).unwrap();
        let occupied = control
            .memory()
            .reserve(control.memory().available() - allowed)
            .unwrap();
        assert!(matches!(
            state.table_state(table, Some(&control)),
            Err(StorageBackendError::Memory(_))
        ));
        assert_eq!(control.memory().used(), occupied.bytes());
        drop(occupied);
        assert_eq!(control.memory().used(), 0);
        allowed += header;
    }
    let control = StorageReadControl::with_limit(1 << 20);
    let table = contents(&control, 1024).into_state(Some(&control)).unwrap();
    control.cancellation().cancel();
    assert!(matches!(
        state.table_state(table, Some(&control)),
        Err(StorageBackendError::Cancelled(_))
    ));
    assert_eq!(control.memory().used(), 0);
}
