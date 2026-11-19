//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::image_io::{ImageDecodeWindow, ImageDecoder};

struct Outcome {
    calls: usize,
    retirements: usize,
    error: Option<(Option<String>, String)>,
}

fn run(
    (tables, indexes): (Vec<Table>, Vec<CatalogIndexRow>),
    steps: usize,
    cancel_at: Option<usize>,
    deny_scratch: bool,
) -> Outcome {
    let memory = MemoryBudget::new(32 << 20);
    let token = CancellationToken::new();
    let original = token.clone();
    let mut decoder = ImageDecoder::new(&memory, &token, move |mut reader| async move {
        let mut input = reader.input();
        validate_with_work(
            tables.iter().map(Table::declaration),
            indexes.iter(),
            std::iter::empty(),
            std::iter::empty(),
            input.memory,
            &original,
            &mut input,
        )
        .await
    })
    .unwrap();
    let pressure = deny_scratch.then(|| memory.reserve(memory.available()).unwrap());
    let mut calls = 0;
    let mut retirements = 0;
    let error = loop {
        if Some(calls) == cancel_at {
            token.cancel();
        }
        let before = memory.used();
        calls += 1;
        match decoder.advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps }) {
            Ok(None) => retirements += usize::from(before > memory.used()),
            Ok(Some(())) => break None,
            Err(error) => break Some((error.sqlstate().map(str::to_owned), error.to_string())),
        }
    };
    drop(decoder);
    drop(pressure);
    assert_eq!(memory.used(), 0);
    Outcome {
        calls,
        retirements,
        error,
    }
}

fn check_columns(missing: usize) -> (Vec<Table>, Vec<CatalogIndexRow>) {
    let mut table = table("checks", 1);
    let mut entries = vec![Expr::Column("id".into()); 4096];
    entries[missing] = Expr::qualified_column("checks", "absent");
    table.columns[0].check = Some(Expr::And(entries));
    (vec![table], Vec::new())
}

#[test]
fn native_constraint_check_columns_yield_at_each_stored_node_before_the_first_error() {
    let first = run(check_columns(0), 1, None, false);
    let last = run(check_columns(4095), 1, None, false);
    let grouped = run(check_columns(4095), 32, None, false);
    assert_eq!(first.error, last.error);
    assert_eq!(grouped.error, last.error);
    assert!(last.error.unwrap().1.contains("missing live column"));
    assert!(last.calls > first.calls + 4095);
    assert!(grouped.calls * 20 < last.calls);
    let cancelled = run(check_columns(4095), 1, Some(first.calls + 32), false);
    assert_eq!(cancelled.error.unwrap().0.as_deref(), Some("57014"));
    let denied = run(check_columns(4095), 1, None, true);
    assert_eq!(denied.error.unwrap().0.as_deref(), Some("53200"));
}

fn wide_foreign_key(duplicate: Option<usize>) -> (Vec<Table>, Vec<CatalogIndexRow>) {
    let (mut tables, mut indexes) = fixture();
    let columns = (0..512)
        .map(|index| format!("column_{index:04}"))
        .collect::<Vec<_>>();
    for table in &mut tables {
        table.columns = columns
            .iter()
            .map(|name| ColumnDef::nullable(name, ColumnType::Integer))
            .collect();
    }
    tables[1].foreign_keys[0].local_columns.clone_from(&columns);
    tables[1].foreign_keys[0].ref_columns.clone_from(&columns);
    let mut keys = columns;
    if let Some(position) = duplicate {
        keys[position] = keys[0].clone();
    }
    indexes[0].columns_json = serde_json::to_string(&keys).unwrap();
    (tables, indexes)
}

#[test]
fn native_constraint_foreign_keys_yield_during_stored_index_matching_and_scratch_retirement() {
    let first = run(wide_foreign_key(Some(1)), 1, None, false);
    let last = run(wide_foreign_key(Some(511)), 1, None, false);
    let grouped = run(wide_foreign_key(Some(511)), 32, None, false);
    assert_eq!(first.error, last.error);
    assert_eq!(last.error, grouped.error);
    assert!(last
        .error
        .unwrap()
        .1
        .contains("duplicate foreign-key index column"));
    assert!(last.calls > first.calls + 510);
    assert!(grouped.calls * 20 < last.calls);
    let cancelled = run(wide_foreign_key(None), 1, Some(first.calls + 100), false);
    assert_eq!(cancelled.error.unwrap().0.as_deref(), Some("57014"));
    let complete = run(wide_foreign_key(None), 1, None, false);
    assert!(complete.error.is_none());
    assert!(complete.retirements >= 512);
    let cancelled = run(wide_foreign_key(None), 1, Some(complete.calls - 30), false);
    assert_eq!(cancelled.error.unwrap().0.as_deref(), Some("57014"));
}
