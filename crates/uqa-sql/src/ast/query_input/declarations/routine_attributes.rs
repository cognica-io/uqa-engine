//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, Input, InputError, JsonToken, Native, Parts, Stored, Values};
use crate::ast::routine_attributes::{
    RoutineAttributeClause, RoutineAttributeClauses, RoutineBodyError, RoutineTransformType,
};

stored_enum!(RoutineAttributeClause {
    As,
    Language,
    Transform,
    Window,
    Volatility,
    Strict,
    Security,
    Leakproof,
    Set,
    Cost,
    Rows,
    Support,
    Parallel,
});

stored_struct!(RoutineAttributeClauses {
    clauses: defaulted,
    invalid_parallel: defaulted,
    transform_types: defaulted,
    body_error: defaulted,
});

stored_enum!(RoutineBodyError {
    Missing,
    Duplicate,
    ExtraAsItems,
});

stored_struct!(RoutineTransformType {
    type_name: required,
    written: required,
});
