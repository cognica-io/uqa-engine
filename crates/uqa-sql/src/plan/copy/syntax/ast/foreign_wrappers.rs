//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Copying, Result, SyntaxCopy};

syntax_struct!(crate::ast::CreateForeignWrapper {
    name,
    functions,
    options
});

syntax_enum!(crate::ast::ForeignWrapperFunctionOption {
    Handler(value0),
    Validator(value0)
});
