//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Copying, Result, SyntaxCopy};

syntax_enum!(crate::ast::IndexKey {
    Column(value0),
    Expression(value0)
});

syntax_struct!(crate::ast::RenameIndexStmt {
    name,
    new_name,
    if_exists
});
