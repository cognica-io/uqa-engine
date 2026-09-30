//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live `PostgreSQL` 18.4 evidence for window frames: the default `RANGE` frame that ends at the last peer of the current row, `ROWS`, `RANGE` and `GROUPS` frames with offsets and exclusions, `first_value`, `last_value` and `nth_value` over peers, empty frames, ranking over peers, `RANGE` offsets on `numeric` and `date` keys, window references and every frame diagnostic.

use uqa_engine::Engine;

fn verify_window_frames(engine: &Engine) {
    crate::pg18_oracle::verify(
        engine,
        include_str!("../../../../tests/parity/pg18/window_frames_oracle.expected.json"),
    );
}

#[test]
fn window_frames_match_postgresql_memory() {
    verify_window_frames(&Engine::new());
}

#[test]
fn window_frames_match_postgresql_sqlite() {
    let directory = tempfile::tempdir().unwrap();
    verify_window_frames(&Engine::open(&directory.path().join("window-frames.db")).unwrap());
}
