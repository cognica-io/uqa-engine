//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rust-only access for installing native faulting providers behind the real C boundary.

#![allow(unsafe_code)]

use crate::{boundary, error, UQAEngine, UQAStatus};

/// Use the actual native session while holding its ordinary exclusive call lease.
///
/// # Safety
/// `engine` must be a live C handle. The caller must obey the same lifetime and synchronization requirements as an ordinary C operation.
pub unsafe fn with_native_engine<T>(
    engine: *mut UQAEngine,
    operation: impl FnOnce(&uqa_engine::Engine) -> T,
) -> Result<T, UQAStatus> {
    let session = &boundary::input(engine).map_err(error::publish)?.session;
    let _call = session.enter().map_err(error::publish)?;
    Ok(operation(&session.native))
}
