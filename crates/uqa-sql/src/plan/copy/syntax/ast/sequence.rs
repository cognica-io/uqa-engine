//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Copying, Result, SyntaxCopy};

syntax_struct!(crate::ast::AutoIncrement {
    kind,
    sequence,
    owner,
    declaration
});

syntax_enum!(crate::ast::AutoIncrementKind {
    Serial,
    IdentityAlways,
    IdentityByDefault,
    Legacy
});

syntax_struct!(crate::ast::AutoIncrementOwner { table, column });
