//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Copying, Result, SyntaxCopy};

syntax_struct!(crate::ast::GrantTypeStmt {
    kind,
    is_grant,
    grant_option,
    grant_option_only,
    privileges,
    names,
    grantees,
    grantor,
    revoke_behavior
});

syntax_enum!(crate::ast::TypePrivilege {
    Usage,
    Unsupported(value0)
});

syntax_enum!(crate::ast::TypeRevokeBehavior { Restrict, Cascade });
