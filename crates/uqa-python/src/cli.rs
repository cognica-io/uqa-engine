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
pub(super) fn usql_main(py: Python<'_>) -> PyResult<u8> {
    let argv: Vec<String> = py.import("sys")?.getattr("argv")?.extract()?;
    let args: Vec<String> = argv.into_iter().skip(1).collect();
    let status = py.detach(move || uqa_cli::run_from_args(&args));
    Ok(u8::from(status != ExitCode::SUCCESS))
}
