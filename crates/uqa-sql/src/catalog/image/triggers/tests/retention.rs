//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn native_trigger_images_keep_captured_generations_after_source_retirement() {
    let memory = MemoryBudget::new(16 << 20);
    let token = CancellationToken::new();
    let mut source = TriggerCatalog::with_budget(&memory);
    source
        .insert_event(RelationIdentity::new("s", "t"), "watch".into(), native())
        .unwrap();
    source
        .insert_event(
            RelationIdentity::new("pg_temp_8", "private"),
            "watch".into(),
            native(),
        )
        .unwrap();
    let capture_memory = MemoryBudget::new(16 << 20);
    let captured = source
        .capture(&ProductionControl::new(&capture_memory, &token, &token))
        .unwrap();
    assert_eq!(captured.len(), 1);
    source.remove(&RelationIdentity::new("s", "t")).unwrap();
    let mut replacement = native();
    replacement.object_id = Some([3; 16]);
    replacement.definition.arguments[0] = "replacement".into();
    source
        .insert_event(RelationIdentity::new("s", "t"), "watch".into(), replacement)
        .unwrap();
    source.clear();
    drop(source);
    assert!(memory.used() > 0);
    let output_memory = MemoryBudget::new(1 << 20);
    assert_eq!(
        collect(captured.image_reader(&output_memory, &token).unwrap(), 13),
        wire::fixture().bytes
    );
    assert_eq!(output_memory.used(), 0);
    drop(captured);
    assert_eq!(memory.used(), 0);
    assert_eq!(capture_memory.used(), 0);
}

#[test]
fn native_trigger_output_borrows_large_arguments_without_corpus_allocation() {
    let mut value = native();
    value.definition.when = None;
    value.definition.arguments = vec!["original".repeat(1 << 20)];
    let source = catalog(value);
    let memory = MemoryBudget::new(64 << 20);
    let token = CancellationToken::new();
    let captured = source
        .capture(&ProductionControl::new(&memory, &token, &token))
        .unwrap();
    let zero = MemoryBudget::new(0);
    let mut buffer = vec![0; 20000];
    let mut bytes = 0;
    let allocation = allocation_counter::measure(|| {
        let mut reader = captured.image_reader(&zero, &token).unwrap();
        loop {
            let count = reader.read(&mut buffer).unwrap();
            assert!(count <= 16384);
            if count == 0 {
                break;
            }
            bytes += count;
        }
    });
    assert!(bytes > 8 << 20);
    assert_eq!(allocation.count_total, 0);
    assert_eq!(zero.used(), 0);
}

#[test]
fn native_trigger_conditions_use_constant_stack_and_admitted_child_readers() {
    let mut wire = wire::fixture();
    wire.bytes.truncate(wire.positions["condition"] + 16);
    wire.bytes.extend(std::iter::repeat_n(15, 10000)); // Native NOT child chain.
    wire.bytes.push(0); // Star leaf; scalar observation is never evaluated.
    std::thread::Builder::new()
        .stack_size(128 * 1024)
        .spawn(move || {
            let token = CancellationToken::new();
            let memory = MemoryBudget::new(64 << 20);
            let source =
                TriggerCatalog::read_image(&mut wire.bytes.as_slice(), &memory, &token).unwrap();
            assert_eq!(encode(&source, 127), wire.bytes);
            drop(source);
            assert_eq!(memory.used(), 0);
        })
        .unwrap()
        .join()
        .unwrap();

    let source = catalog(native());
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(1 << 20);
    let captured = source
        .capture(&ProductionControl::new(&memory, &token, &token))
        .unwrap();
    let ample = MemoryBudget::new(1 << 20);
    let expected = collect(captured.image_reader(&ample, &token).unwrap(), 20000);
    assert_eq!(ample.used(), 0);
    let exact = MemoryBudget::new(ample.peak());
    assert_eq!(
        collect(captured.image_reader(&exact, &token).unwrap(), 1),
        expected
    );
    assert_eq!(exact.used(), 0);
    for limit in [0, ample.peak() - 1] {
        let memory = MemoryBudget::new(limit);
        let mut reader = captured.image_reader(&memory, &token).unwrap();
        assert_eq!(
            reader.read(&mut [0; 4096]).unwrap_err().sqlstate(),
            Some("53200")
        );
        assert_eq!(memory.used(), 0);
        assert!(reader.read(&mut []).is_err());
    }
}

#[test]
fn native_trigger_output_cancellation_retires_children_and_remains_terminal() {
    let source = catalog(native());
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(1 << 20);
    let captured = source
        .capture(&ProductionControl::new(&memory, &token, &token))
        .unwrap();
    let scratch = MemoryBudget::new(1 << 20);
    let mut reader = captured.image_reader(&scratch, &token).unwrap();
    let position = wire::fixture().positions["condition"] + 17;
    for _ in 0..position {
        assert_eq!(reader.read(&mut [0; 1]).unwrap(), 1);
    }
    assert!(scratch.used() > 0);
    token.cancel();
    assert!(reader.read(&mut [0; 32]).is_err());
    assert_eq!(scratch.used(), 0);
    assert!(reader.read(&mut []).is_err());
    assert!(captured.image_reader(&scratch, &token).is_err());
    let token = CancellationToken::new();
    let mut reader = captured.image_reader(&scratch, &token).unwrap();
    assert_eq!(
        reader.read(&mut [0; 4096]).unwrap(),
        wire::fixture().bytes.len()
    );
    assert_eq!(scratch.used(), 0);
    token.cancel();
    assert_eq!(reader.read(&mut [0; 32]).unwrap(), 0);
}
