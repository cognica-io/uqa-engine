//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Copying, Result, SyntaxCopy};

syntax_enum!(crate::ast::PatternOperator {
    Like,
    NotLike,
    ILike,
    NotILike,
    Regex,
    NotRegex,
    RegexInsensitive,
    NotRegexInsensitive,
    StartsWith
});
