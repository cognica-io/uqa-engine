//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered simplification of conditional expressions.

use super::{constants::literal_value_with_control, OptimizerConfig, SQLError, ScalarExpr, Value};
use uqa_core::memory::ProductionControl;
use uqa_sql::plan::OwnedScalarExpr;

mod boolean;
mod owned;
pub(super) use boolean::optimize as optimize_owned_boolean;
pub(super) use owned::{
    optimize_case as optimize_owned_case, optimize_coalesce as optimize_owned_coalesce,
};
