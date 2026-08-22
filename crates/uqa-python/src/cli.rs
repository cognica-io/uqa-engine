//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Python console entry point for `usql`.

use std::process::ExitCode;

use pyo3::prelude::*;

#[pyfunction]
#[pyo3(name = "_usql_main")]
pub(super) fn usql_main(py: Python<'_>) -> u8 {
    let status = py.detach(uqa_cli::run_from_env);
    u8::from(status != ExitCode::SUCCESS)
}
