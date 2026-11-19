//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, Input, InputError, JsonToken, Native, Parts, Stored, Values};
use crate::ast::locking::{LockTableStmt, LockTableTarget, TableLockMode};

stored_struct!(LockTableStmt {
    targets: required,
    mode: required,
    nowait: required,
});

stored_struct!(LockTableTarget {
    name: required,
    include_descendants: required,
});

stored_enum!(TableLockMode {
    AccessShare,
    RowShare,
    RowExclusive,
    ShareUpdateExclusive,
    Share,
    ShareRowExclusive,
    Exclusive,
    AccessExclusive,
});
