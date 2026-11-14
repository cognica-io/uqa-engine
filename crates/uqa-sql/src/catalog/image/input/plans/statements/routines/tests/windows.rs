//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::image::tests::byte_windows::{assert_retirement, read_windowed, WINDOWS};

#[test]
fn native_routine_windows_preserve_overloads_bodies_and_destination_edits() {
    let bytes = encode(&fixture());
    for (width, steps) in WINDOWS {
        let memory = MemoryBudget::new(64 << 20);
        let mutations = MemoryBudget::new(16 << 20);
        let token = CancellationToken::new();
        let mut value = read_windowed(
            RoutineRegistry::image_input(&memory, &mutations, &token).unwrap(),
            &bytes,
            width,
            steps,
        );
        assert_eq!(encode(&value), bytes);
        let retained = value.clone();
        let RoutineBody::Bound(body) = &value["public.f"][0].body else {
            panic!("bound routine");
        };
        let body = body.clone();
        let full = memory.reserve(memory.available()).unwrap();
        token.cancel();
        value
            .edit("public.f", |values| {
                values.truncate(1);
                Ok(())
            })
            .unwrap();
        assert_eq!(value["public.f"].len(), 1);
        assert_eq!(retained["public.f"].len(), 6);
        assert_eq!(encode(&retained), bytes);
        drop((value, retained));
        assert_eq!(mutations.used(), 0);
        assert!(body.is_retained_by(&memory));
        assert_eq!(allocation_counter::measure(|| drop(body)).count_total, 0);
        drop(full);
        assert_eq!(memory.used(), 0);
    }
    assert_retirement(&bytes, RoutineRegistry::image_input);
}

#[test]
fn native_routine_windows_preserve_large_lazy_source_and_identity_claims() {
    let original = routine(
        "CREATE FUNCTION f() RETURNS integer LANGUAGE SQL AS 'unexamined'",
        1,
    );
    let mut definition = original.def.clone();
    definition.body = FunctionBody::Source("unparsed 한λ😀".repeat(1025));
    let source = RoutineRegistry::try_from_iter([(
        "public.f".into(),
        vec![SQLUserFunction::new(definition, RoutineBody::Source).shared()],
    )])
    .unwrap();
    let bytes = encode(&source);
    for (width, steps) in WINDOWS {
        let memory = MemoryBudget::new(64 << 20);
        let token = CancellationToken::new();
        let value = read_windowed(
            RoutineRegistry::image_input(&memory, &memory, &token).unwrap(),
            &bytes,
            width,
            steps,
        );
        assert_eq!(encode(&value), bytes);
        assert!(matches!(value["public.f"][0].body, RoutineBody::Source));
        drop(value);
        assert_eq!(memory.used(), 0);
    }
    let malformed =
        RoutineRegistry::try_from_iter([("public.f".into(), vec![original.clone(), original])])
            .unwrap();
    let bytes = encode(&malformed);
    let memory = MemoryBudget::new(64 << 20);
    let token = CancellationToken::new();
    let mut input = RoutineRegistry::image_input(&memory, &memory, &token).unwrap();
    let mut reader = bytes.as_slice();
    loop {
        match input.advance_with_window(
            &mut reader,
            1,
            uqa_core::image_io::ImageDecodeWindow { bytes: 1, steps: 3 },
        ) {
            Ok(None) => {}
            Ok(Some(_)) => panic!("duplicate routine identity accepted"),
            Err(_) => break,
        }
    }
    assert_eq!(memory.used(), 0);
    let before = reader.len();
    assert!(input.advance(&mut reader, 1).is_err());
    assert_eq!(reader.len(), before);
}

#[test]
fn native_routine_windows_keep_original_and_bound_forests_on_small_stacks() {
    let bytes = super::resources::deep_image();
    std::thread::Builder::new()
        .stack_size(128 << 10)
        .spawn(move || {
            let memory = MemoryBudget::new(128 << 20);
            let token = CancellationToken::new();
            let value = read_windowed(
                RoutineRegistry::image_input(&memory, &memory, &token).unwrap(),
                &bytes,
                17,
                3,
            );
            let captured = value
                .capture(&ProductionControl::new(&memory, &token, &token))
                .unwrap();
            assert_eq!(
                drain(captured.image_reader(&memory, &token).unwrap()),
                bytes
            );
            drop(captured);
            let full = memory.reserve(memory.available()).unwrap();
            assert_eq!(allocation_counter::measure(|| drop(value)).count_total, 0);
            drop(full);
            assert_eq!(memory.used(), 0);
        })
        .unwrap()
        .join()
        .unwrap();
}
