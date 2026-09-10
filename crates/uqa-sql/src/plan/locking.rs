//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical relation row marks selected by SQL locking analysis.

use crate::ast::{LockStrength, LockWait};

#[derive(Clone, Debug)]
pub struct ResolvedRowLock {
    pub qualifier: String,
    pub storage_name: String,
    pub display_name: String,
    pub strength: LockStrength,
    pub wait: LockWait,
    pub identity_source: bool,
}
