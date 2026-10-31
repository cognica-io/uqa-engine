//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Copying, Result, SyntaxCopy};

syntax_enum!(crate::ast::SchemaAuthorization {
    Role(value0),
    CurrentUser,
    SessionUser
});
