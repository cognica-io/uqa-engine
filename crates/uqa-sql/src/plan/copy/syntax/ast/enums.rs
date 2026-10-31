//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Copying, Result, SyntaxCopy};

syntax_struct!(crate::ast::AlterEnum { name, action });

syntax_enum!(crate::ast::AlterEnumAction {
    AddValue { label, if_not_exists, neighbor },
    RenameValue { old, new }
});

syntax_struct!(crate::ast::CreateEnum { name, labels });

syntax_struct!(crate::ast::EnumNeighbor { label, after });
