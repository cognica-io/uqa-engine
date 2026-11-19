//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::byte_windows::{assert_retirement, read_windowed, WINDOWS};
use super::*;

#[test]
fn builtin_authority_input_bounds_acl_fields_and_retires_partial_catalogs() {
    let bytes = images()[0].clone();
    let token = CancellationToken::new();
    for (width, steps) in WINDOWS {
        let memory = MemoryBudget::new(1 << 20);
        let restored = read_windowed(
            BuiltinRoutineSecurities::image_input(&memory, &memory, &token).unwrap(),
            &bytes,
            width,
            steps,
        );
        assert!(restored == routine_fixture());
        let captured = restored
            .capture(&ProductionControl::new(&memory, &token, &token))
            .unwrap();
        assert_eq!(streamed(captured.image_reader(&token).unwrap(), 7), bytes);
        drop((captured, restored));
        assert_eq!(memory.used(), 0);
    }
    assert_retirement(&bytes, BuiltinRoutineSecurities::image_input);
}

#[test]
fn system_authority_input_bounds_columns_and_retires_partial_catalogs() {
    let bytes = images()[1].clone();
    let token = CancellationToken::new();
    for (width, steps) in WINDOWS {
        let memory = MemoryBudget::new(1 << 20);
        let restored = read_windowed(
            SystemRelationSecurities::image_input(&memory, &memory, &token).unwrap(),
            &bytes,
            width,
            steps,
        );
        assert!(restored == system_fixture());
        let captured = restored
            .capture(&ProductionControl::new(&memory, &token, &token))
            .unwrap();
        assert_eq!(streamed(captured.image_reader(&token).unwrap(), 7), bytes);
        drop((captured, restored));
        assert_eq!(memory.used(), 0);
    }
    assert_retirement(&bytes, SystemRelationSecurities::image_input);
}
