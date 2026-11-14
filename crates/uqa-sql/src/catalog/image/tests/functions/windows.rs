//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::image::input::window_tests::{assert_retirement, read_windowed, WINDOWS};

#[test]
fn function_binding_input_bounds_original_dispatch_and_deferred_diagnostics() {
    let mut source = retained_binding();
    source.name = "한🙂".repeat(1025);
    for error in [
        source.resolution_error.clone(),
        Some(Box::new(FunctionResolutionError::UndefinedFunction {
            signature: "missing🙂".repeat(1025),
        })),
    ] {
        source.resolution_error = error;
        let expected = encode(&source);
        for (width, steps) in WINDOWS {
            let memory = MemoryBudget::new(16 << 20);
            let token = CancellationToken::new();
            let value = read_windowed(
                FunctionBinding::image_input(&memory, &token).unwrap(),
                &expected,
                width,
                steps,
            );
            assert_eq!(*value, source);
            token.cancel();
            let full = memory.reserve(memory.available()).unwrap();
            assert_eq!(encode(&value), expected);
            drop((value, full));
            assert_eq!(memory.used(), 0);
        }
        assert_retirement(&expected, FunctionBinding::image_input);
    }
}
