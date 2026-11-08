//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::vector_index::{
    image::{read_vector_image, tests::encode},
    MemoryVectorIndex,
};
use uqa_core::memory::BudgetedString;

fn field(name: &str, control: &StorageReadControl) -> Budgeted<String> {
    let mut name_owner = BudgetedString::new(control.memory());
    name_owner.push_str(name).unwrap();
    let (name, memory) = name_owner.into_parts();
    Budgeted::new(name, memory)
}

fn decoded(control: &StorageReadControl) -> Box<dyn VectorIndex> {
    let mut original = MemoryVectorIndex::new(2);
    original.add(1, vec![1.0, -0.0]).unwrap();
    original.add(u64::MAX, vec![0.0, 1.0]).unwrap();
    read_vector_image(&mut encode(&original).as_slice(), control).unwrap()
}

#[test]
fn native_vector_import_retains_original_field_buffers_and_writable_capture() {
    let registry = MemoryCaptureRegistry::new();
    let input = StorageReadControl::with_limit(1 << 20);
    let name = field("embedding", &input);
    let pointer = name.as_ptr();
    let original = decoded(&input);
    let expected = encode(&*original);
    let indexes = MemoryVectorIndexesBuilder::new(&registry, &input)
        .add_index(name, original)
        .unwrap()
        .finish()
        .unwrap();
    assert_eq!(indexes.keys().next().unwrap().as_ptr(), pointer);
    let source = registry.cell(indexes).unwrap();
    let output = StorageReadControl::with_limit(1 << 20);
    let cut = registry.capture(&output).unwrap();
    let selected = source.captured(&cut).unwrap();
    let occupied = input.memory().reserve(input.memory().available()).unwrap();
    input.cancellation().cancel();
    source
        .write()
        .live_mut()
        .unwrap()
        .get_mut("embedding")
        .unwrap()
        .add(2, vec![0.5, 0.5])
        .unwrap();
    source
        .write()
        .live_mut()
        .unwrap()
        .insert("later".into(), Box::new(MemoryVectorIndex::new(3)))
        .unwrap();
    assert_eq!(
        source.read().keys().map(String::as_str).collect::<Vec<_>>(),
        ["embedding", "later"]
    );
    drop(source);
    let image = selected.image("embedding", &cut).unwrap();
    drop((selected, cut, registry));
    let working = StorageReadControl::with_limit(4096);
    let mut bytes = Vec::new();
    image.write_image(&mut bytes, &working).unwrap();
    assert_eq!(bytes, expected);
    drop(image);
    assert_eq!(input.memory().used(), occupied.bytes());
    drop(occupied);
    assert_eq!(input.memory().used(), 0);
    assert_eq!(output.memory().used(), 0);
    assert_eq!(working.memory().used(), 0);
}

#[test]
fn native_vector_import_rejects_duplicate_foreign_short_and_cancelled_names() {
    let registry = MemoryCaptureRegistry::new();
    for invalid in 0..4 {
        let input = StorageReadControl::with_limit(1 << 20);
        let foreign = StorageReadControl::with_limit(1 << 20);
        let builder = MemoryVectorIndexesBuilder::new(&registry, &input)
            .add_index(field("first", &input), decoded(&input))
            .unwrap();
        let name = match invalid {
            0 => field("first", &input),
            1 => field("second", &foreign),
            2 => Budgeted::new("second".into(), input.memory().empty_reservation()),
            _ => field("second", &input),
        };
        let index = decoded(&input);
        if invalid == 3 {
            input.cancellation().cancel();
        }
        assert!(builder.add_index(name, index).is_err());
        assert_eq!(input.memory().used(), 0);
        assert_eq!(foreign.memory().used(), 0);
    }
    let input = StorageReadControl::with_limit(0);
    let builder = MemoryVectorIndexesBuilder::new(&registry, &input);
    input.cancellation().cancel();
    assert!(builder.finish().is_err());
    assert_eq!(input.memory().used(), 0);
}

#[test]
fn native_vector_registration_admission_failure_retires_all_partial_membership() {
    let registry = MemoryCaptureRegistry::new();
    let input = StorageReadControl::with_limit(1 << 20);
    let first = MemoryVectorIndexesBuilder::new(&registry, &input)
        .add_index(field("first", &input), decoded(&input))
        .unwrap();
    let name = field("second", &input);
    let index = decoded(&input);
    let remainder = input
        .memory()
        .reserve(input.memory().limit() - input.memory().used())
        .unwrap();
    let reserved = remainder.bytes();
    assert!(first.add_index(name, index).is_err());
    assert_eq!(input.memory().used(), reserved);
    drop(remainder);
    assert_eq!(input.memory().used(), 0);
}
