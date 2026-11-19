//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::byte_windows::{assert_retirement, read_windowed, WINDOWS};
use super::*;

#[test]
fn foreign_server_input_bounds_options_and_retires_partial_fields() {
    let mut value = server_value();
    value.options.insert("a".into(), "한🙂".repeat(1025));
    let bytes = encode(&singleton("s", value));
    let token = CancellationToken::new();
    for (width, steps) in WINDOWS {
        let memory = MemoryBudget::new(1 << 20);
        let restored = read_windowed(
            ForeignServers::image_input(&memory, &memory, &token).unwrap(),
            &bytes,
            width,
            steps,
        );
        assert_eq!(encode(&restored), bytes);
        drop(restored);
        assert_eq!(memory.used(), 0);
    }
    assert_retirement(&bytes, ForeignServers::image_input);
}

#[test]
fn graph_oid_input_bounds_label_fields_and_retires_identity_claims() {
    let bytes = encode(&singleton("g", graph_value()));
    let token = CancellationToken::new();
    for (width, steps) in WINDOWS {
        let memory = MemoryBudget::new(1 << 20);
        let restored = read_windowed(
            GraphOidRegistry::image_input(&memory, &memory, &token).unwrap(),
            &bytes,
            width,
            steps,
        );
        assert_eq!(encode(&restored), bytes);
        drop(restored);
        assert_eq!(memory.used(), 0);
    }
    assert_retirement(&bytes, GraphOidRegistry::image_input);
}
