//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Copying, Result, SyntaxCopy};

syntax_enum!(crate::ast::RoutineAttributeClause {
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
    Parallel
});

syntax_struct!(crate::ast::RoutineAttributeClauses {
    clauses,
    invalid_parallel,
    transform_types,
    body_error
});

syntax_enum!(crate::ast::RoutineBodyError {
    Missing,
    Duplicate,
    ExtraAsItems
});

syntax_struct!(crate::ast::RoutineTransformType { type_name, written });
