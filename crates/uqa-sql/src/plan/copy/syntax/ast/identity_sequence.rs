//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Copying, Result, SyntaxCopy};

syntax_struct!(crate::ast::DeferredSQLError {
    sqlstate,
    message,
    detail,
    hint
});

syntax_struct!(crate::ast::IdentitySequenceDeclaration {
    name,
    persistence,
    sequence,
    error
});

syntax_struct!(crate::ast::IdentitySequenceName { schema, name });
