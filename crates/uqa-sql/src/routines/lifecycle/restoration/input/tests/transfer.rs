//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn canonical_routine_group_transfer_preserves_native_names_overloads_and_declarations() {
    for consumed in 0..=3 {
        let native = MemoryBudget::new(1 << 20);
        let work = MemoryBudget::new(1 << 20);
        let token = CancellationToken::new();
        let groups = ["z", "a", "m"]
            .into_iter()
            .enumerate()
            .map(|(index, name)| {
                (
                    name.into(),
                    true,
                    vec![definition(
                        name,
                        u8::try_from(index + 1).unwrap(),
                        &["integer", "varchar(9)"],
                    )],
                )
            })
            .collect();
        let (output, _) =
            complete(&mut decoder(&groups, &native, &work, &token, &token), 1).unwrap();
        let expected = output
            .definitions()
            .map(|(name, values)| {
                (
                    name.as_ptr(),
                    values.as_ptr(),
                    values[0].params.as_ptr(),
                    values[0].params[0].type_name.as_ptr(),
                )
            })
            .collect::<Vec<_>>();
        let before = native.used();
        let hold = native.reserve(native.available()).unwrap();
        let mut input = output.into_groups();
        let mut retained = Vec::with_capacity(consumed);
        token.cancel();
        let allocation = allocation_counter::measure(|| {
            for (index, &(name_pointer, vector_pointer, parameters, type_name)) in
                expected.iter().take(consumed).enumerate()
            {
                let CanonicalRoutineGroup {
                    name,
                    definitions: values,
                    ..
                } = input.next().unwrap();
                assert_eq!(
                    (
                        name.as_ptr(),
                        values.as_ptr(),
                        values[0].params.as_ptr(),
                        values[0].params[0].type_name.as_ptr()
                    ),
                    (name_pointer, vector_pointer, parameters, type_name)
                );
                assert!(name.is_retained_by(&native));
                assert_eq!(name.reserved_bytes(), name.capacity());
                assert_eq!(
                    native.used(),
                    hold.bytes() + before - (index + 1) * OwnedMap::<String, Group>::entry_bytes()
                );
                retained.push((name, values));
            }
        });
        assert_eq!(allocation.count_total, 0);
        assert_eq!(input.len(), 3 - consumed);
        drop((input, hold));
        assert_eq!(native.used() == 0, consumed == 0);
        assert_eq!(work.used(), 0);
        for (name, values) in &retained {
            assert_eq!(name.as_str(), values[0].name);
        }
        drop(retained);
        assert_eq!(native.used(), 0);
    }
}
