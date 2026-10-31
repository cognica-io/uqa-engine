//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Copying, Result, SyntaxCopy};

syntax_enum!(crate::ast::EnumFunctionOperation {
    First,
    Last,
    Range,
    BoundedRange,
    Compare,
    Equal,
    NotEqual,
    Less,
    Greater,
    LessEqual,
    GreaterEqual,
    Smaller,
    Larger,
    Hash,
    ExtendedHash
});
