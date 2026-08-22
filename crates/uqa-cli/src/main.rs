//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `usql` binary entry point.

use std::process::ExitCode;

fn main() -> ExitCode {
    uqa_cli::run_from_env()
}
