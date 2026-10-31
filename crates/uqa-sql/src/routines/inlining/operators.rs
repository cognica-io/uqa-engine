//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Mutability of the exact overload selected for a SQL binary operator.

use super::RoutineInliningContext;
use crate::{
    ast::{BinaryOp, FunctionBinding, FunctionDispatch, FunctionVolatility},
    RowSchema, SQLParam, ScalarExpr,
};

mod production;
pub(super) use production::{
    comparisons_with_control, concatenation_volatility_with_control, optional_type,
    volatility_with_control,
};
