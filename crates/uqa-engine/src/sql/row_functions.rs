//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retrieval function binding to the live Engine state.

use crate::{Engine, ScoredEntry};
use uqa_sql::{
    registry::{lookup, FunctionKind},
    SQLError, SQLParam, ScalarExpr,
};

mod dispatch;
