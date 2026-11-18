//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::schema::{RecordDescriptor, RecordFieldMetadata, RecordMetadata, SchemaMap};
use uqa_core::memory::ProductionVec;

fn record(width: usize, depth: usize, control: &ProductionControl<'_>) -> Arc<RecordDescriptor> {
    let ty = nested_type(depth);
    let types =
        RecordFields::from_types_with_control(std::iter::repeat_n(Some(&ty), width), control)
            .unwrap();
    let mut names = ProductionVec::new(*control);
    let mut metadata = ProductionVec::new(*control);
    for _ in 0..width {
        names
            .push_produced(control.copy_text("retained_field").unwrap())
            .unwrap();
        metadata
            .push_copy(RecordFieldMetadata {
                collation_oid: 951,
                sql_unknown: false,
            })
            .unwrap();
    }
    RecordDescriptor::from_parts_with_control(
        types,
        Some(names.finish().unwrap()),
        metadata.finish().unwrap(),
        control,
    )
    .unwrap()
}

#[test]
fn nested_schema_metadata_keeps_independent_descriptors_and_diagnostic_generations() {
    let budget = MemoryBudget::new(1 << 24);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let descriptor = record(128, 32, &control);
    let descriptor_bytes = budget.used();
    let mut source = RowSchema::with_column_metadata_with_control(
        Some("q"),
        [
            ("record", Some(&ColumnType::Record), None),
            ("text", Some(&ColumnType::Text), Some(951)),
        ],
        &control,
    )
    .unwrap();
    Arc::get_mut(&mut source.index).unwrap().cold.record_fields = SchemaMap::from_produced(
        [Ok((
            control.finish(0, control.empty_reservation()).unwrap(),
            control
                .finish(
                    RecordMetadata {
                        types: Some(descriptor.types.clone()),
                        descriptor: Some(descriptor.clone()),
                    },
                    control.empty_reservation(),
                )
                .unwrap(),
        ))],
        &control,
    )
    .unwrap();
    let diagnostic =
        RowSchema::with_source_diagnostic_namespace_with_control(&source, "q", &control).unwrap();
    let denied =
        RowSchema::without_lateral_access_with_control(&diagnostic, "q", &control).unwrap();
    let mut unavailable =
        RowSchema::with_unavailable_names_with_control(&denied, &diagnostic, true, &control)
            .unwrap();
    let original_fields = descriptor.types.as_ptr();
    let held = budget.used();
    let reader = unavailable.clone();
    let mut state = unavailable.take_retirement();
    assert!(state.advance());
    assert_eq!(budget.used(), held);
    drop((state, unavailable, source, diagnostic, denied));
    assert_eq!(reader.record_fields(0).unwrap().as_ptr(), original_fields);
    let mut reader = reader;
    let mut state = reader.take_retirement();
    while !state.advance() {}
    drop((state, reader));
    assert_eq!(budget.used(), descriptor_bytes);
    assert_eq!(descriptor.types.len(), 128);
    assert_eq!(descriptor.types.as_ptr(), original_fields);
    assert_eq!(descriptor.position("retained_field"), Some(0));
    assert_eq!(
        allocation_counter::measure(|| drop(descriptor)).count_total,
        0
    );
    assert_eq!(budget.used(), 0);
}

#[test]
fn last_schema_descriptor_retires_native_types_and_names_one_work_unit_at_a_time() {
    let budget = MemoryBudget::new(1 << 24);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let descriptor = record(128, 32, &control);
    let mut schema = schema(1, 1, &control);
    Arc::get_mut(&mut schema.index).unwrap().cold.record_fields = SchemaMap::from_produced(
        [Ok((
            control.finish(0, control.empty_reservation()).unwrap(),
            control
                .finish(
                    RecordMetadata {
                        types: Some(descriptor.types.clone()),
                        descriptor: Some(descriptor),
                    },
                    control.empty_reservation(),
                )
                .unwrap(),
        ))],
        &control,
    )
    .unwrap();
    let exhausted = budget.reserve(budget.available()).unwrap();
    let mut retirement = schema.take_retirement();
    let mut steps = 0;
    loop {
        let mut complete = false;
        let allocation = allocation_counter::measure(|| complete = retirement.advance());
        assert_eq!(allocation.count_total, 0, "{allocation:?}");
        assert!(allocation.count_current >= -96, "{allocation:?}");
        steps += 1;
        if complete {
            break;
        }
        assert!(steps < 30_000);
    }
    assert!(steps > 128 * 32);
    assert_eq!(budget.used(), exhausted.bytes());
    drop((retirement, schema, exhausted));
    assert_eq!(budget.used(), 0);
}
