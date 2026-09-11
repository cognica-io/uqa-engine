//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Session portal declaration and native row-streaming execution.

pub mod context;
pub mod declaration;
pub mod worker;

pub use context::{SessionPortalCommandDeclaration, SessionPortalDeclaration};
