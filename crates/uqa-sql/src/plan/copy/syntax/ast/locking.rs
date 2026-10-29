//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Copying, Result, SyntaxCopy};

syntax_enum!(crate::ast::LockStrength {
    ForKeyShare,
    ForShare,
    ForNoKeyUpdate,
    ForUpdate
});

syntax_struct!(crate::ast::LockTableStmt {
    targets,
    mode,
    nowait
});

syntax_struct!(crate::ast::LockTableTarget {
    name,
    include_descendants
});

syntax_enum!(crate::ast::LockWait {
    Block,
    SkipLocked,
    NoWait
});

syntax_struct!(crate::ast::LockingClause {
    strength,
    wait,
    relations
});

syntax_enum!(crate::ast::TableLockMode {
    AccessShare,
    RowShare,
    RowExclusive,
    ShareUpdateExclusive,
    Share,
    ShareRowExclusive,
    Exclusive,
    AccessExclusive
});
