//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain and rewrite original rows after ALTER action preparation.

mod hierarchy;
mod rewrite;
mod source;
pub(super) use rewrite::TypeRewrites;
