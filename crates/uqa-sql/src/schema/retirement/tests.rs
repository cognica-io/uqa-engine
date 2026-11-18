//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{schema::RecordFields, ColumnType};
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};

mod metadata;

std::thread_local! {
    pub(super) static FIELD_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn nested_type(depth: usize) -> ColumnType {
    (0..depth).fold(ColumnType::Text, |child, _| {
        ColumnType::Array(Box::new(child))
    })
}

fn schema(width: usize, depth: usize, control: &ProductionControl<'_>) -> RowSchema {
    let ty = nested_type(depth);
    let names = (0..width)
        .map(|i| format!("column_{i}"))
        .collect::<Vec<_>>();
    RowSchema::with_column_metadata_with_control(
        Some("q"),
        names.iter().map(|name| (name.as_str(), Some(&ty), None)),
        control,
    )
    .unwrap()
}

#[test]
fn ordinary_row_schema_drop_releases_shared_and_final_owners_without_stepped_field_visits() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let original = schema(32, 16, &control);
    let reader = original.clone();
    let columns = reader.columns().as_ptr();
    let remaining = budget.reserve(budget.limit() - budget.used()).unwrap();

    FIELD_VISITS.set(0);
    let shared = allocation_counter::measure(|| drop(original));
    assert_eq!(shared.count_total, 0);
    assert_eq!(reader.columns().as_ptr(), columns);
    assert_eq!(reader.qualified_slot("q", "column_31"), Some(31));
    assert_eq!(budget.used(), budget.limit());
    assert_eq!(FIELD_VISITS.get(), 0);
    assert_eq!(allocation_counter::measure(|| drop(reader)).count_total, 0);
    assert_eq!(budget.used(), remaining.bytes());
    assert_eq!(FIELD_VISITS.get(), 0);
    drop(remaining);

    let mut original = schema(32, 16, &control);
    FIELD_VISITS.set(0);
    let mut retirement = original.take_retirement();
    while !retirement.advance() {}
    assert!(FIELD_VISITS.get() > 0);
    assert_eq!(budget.used(), 0);
}

#[test]
fn row_schema_retirement_preserves_readers_and_releases_native_fields_without_allocation() {
    let budget = MemoryBudget::new(1 << 24);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut original = schema(128, 16, &control);
    let mut reader = original.clone();
    let names = reader.columns().as_ptr();
    let used = budget.used();
    let mut first = original.take_retirement();
    assert!(first.advance());
    drop((first, original));
    assert_eq!(reader.columns().as_ptr(), names);
    assert_eq!(reader.qualified_slot("q", "column_127"), Some(127));
    assert_eq!(budget.used(), used);

    let exhausted = budget.reserve(budget.limit() - budget.used()).unwrap();
    let mut state = None;
    let allocation = allocation_counter::measure(|| state = Some(reader.take_retirement()));
    assert_eq!(allocation.count_total, 0, "{allocation:?}");
    assert_eq!(budget.used(), budget.limit());
    let mut state = state.unwrap();
    let mut steps = 0;
    loop {
        let mut complete = false;
        let allocation = allocation_counter::measure(|| complete = state.advance());
        assert_eq!(allocation.count_total, 0, "{allocation:?}");
        assert!(allocation.count_current >= -96, "{allocation:?}");
        steps += 1;
        if complete {
            break;
        }
        assert!(steps < 30_000);
    }
    assert!(steps > 128 * 16);
    assert_eq!(budget.used(), exhausted.bytes());
    assert!(state.advance());
    drop((state, reader, exhausted));
    assert_eq!(budget.used(), 0);
}

#[test]
fn every_row_schema_retirement_prefix_keeps_independent_reservations() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let sibling = budget.reserve(37).unwrap();
    let mut count = 0;
    let mut original = schema(3, 4, &control);
    let mut state = original.take_retirement();
    while !state.advance() {
        count += 1;
    }
    drop((state, original));
    for prefix in 0..=count {
        let mut original = schema(3, 4, &control);
        let mut state = original.take_retirement();
        for _ in 0..prefix {
            assert!(!state.advance());
        }
        let allocation = allocation_counter::measure(|| drop((state, original)));
        assert_eq!(allocation.count_total, 0, "prefix {prefix}: {allocation:?}");
        assert_eq!(budget.used(), sibling.bytes(), "prefix {prefix}");
    }
}

#[test]
fn owned_record_fields_keep_the_original_vector_and_shared_external_fields_keep_their_identity() {
    let fields = vec![Some(nested_type(512)); 16];
    let pointer = fields.as_ptr();
    let owned: RecordFields = fields.into();
    assert_eq!(owned.as_ptr(), pointer);
    let reader = owned.clone();
    assert!(owned.shares_storage(&reader));
    assert_eq!(allocation_counter::measure(|| drop(owned)).count_total, 0);
    assert_eq!(reader.as_ptr(), pointer);
    assert_eq!(reader.len(), 16);
    assert_eq!(allocation_counter::measure(|| drop(reader)).count_total, 0);

    let external: Arc<[Option<ColumnType>]> = vec![Some(nested_type(512)); 16].into();
    let pointer = external.as_ptr();
    let first: RecordFields = external.clone().into();
    let second: RecordFields = external.clone().into();
    assert!(first.shares_storage(&second));
    assert_eq!(first.as_ptr(), pointer);
    drop((first, second));
    assert_eq!(external.as_ptr(), pointer);
    assert_eq!(external.len(), 16);
}

#[test]
fn concurrent_schema_owners_select_one_original_generation_for_retirement() {
    let budget = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let original = schema(8, 8, &control);
    let barrier = Arc::new(std::sync::Barrier::new(4));
    let mut copies = vec![original.clone(); 3];
    copies.push(original);
    let threads = copies
        .into_iter()
        .map(|mut schema| {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                let state = schema.take_retirement();
                let last = state.fields.is_some();
                drop((state, schema));
                last
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(
        threads
            .into_iter()
            .map(|thread| usize::from(thread.join().unwrap()))
            .sum::<usize>(),
        1
    );
    assert_eq!(budget.used(), 0);
}

#[test]
fn deep_analyzed_sources_and_type_metadata_retire_on_a_small_stack() {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let base = RowSchema::with_types(vec!["value".into()], vec![Some(nested_type(8192))]);
            let leaf = || crate::plan::AnalyzedSource {
                schema: base.clone(),
                children: None,
            };
            let mut source = leaf();
            for _ in 0..8192 {
                source = crate::plan::AnalyzedSource {
                    schema: base.clone(),
                    children: Some(Box::new((source, leaf()))),
                };
            }
            drop(base);
            let allocation = allocation_counter::measure(|| drop(source));
            assert_eq!(allocation.count_total, 0, "{allocation:?}");
        })
        .unwrap()
        .join()
        .unwrap();
}
