//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::super::byte_windows::{assert_retirement, read_windowed, WINDOWS};
use super::*;

#[test]
fn foreign_wrapper_input_bounds_callback_fields_and_retires_partial_bindings() {
    let token = CancellationToken::new();
    for value_binding in values() {
        let mut function = callback();
        function.binding.invocation = Some(Box::new(invocation(RoutineVariadicMode::Expanded {
            parameter_index: 2,
        })));
        function.binding.value_binding = value_binding;
        let mut value = ForeignWrapperDefinition {
            handler: ForeignWrapperHandler::Function(function.clone()),
            validator: Some(function),
            ..wrapper()
        };
        value.options[0].1 = "한🙂".repeat(1025);
        let bytes = encode(&singleton("w", value));
        for (width, steps) in WINDOWS {
            let memory = MemoryBudget::new(1 << 20);
            let restored = read_windowed(
                ForeignWrappers::image_input(&memory, &memory, &token).unwrap(),
                &bytes,
                width,
                steps,
            );
            assert_eq!(encode(&restored), bytes);
            drop(restored);
            assert_eq!(memory.used(), 0);
        }
        assert_retirement(&bytes, ForeignWrappers::image_input);
    }
}
