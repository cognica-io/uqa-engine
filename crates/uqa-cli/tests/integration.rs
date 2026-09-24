//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Consolidated CLI integration tests.

#[path = "encrypted.rs"]
mod encrypted;
#[path = "history.rs"]
mod history;
#[path = "parity.rs"]
mod parity;

fn binary_path() -> std::path::PathBuf {
    std::env::var_os("CARGO_BIN_EXE_usql")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_BIN_EXE_usql")))
}
