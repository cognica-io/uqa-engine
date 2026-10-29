//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Copying, Result, SyntaxCopy};

syntax_struct!(crate::ast::ReferencedPartitionConstraint {
    partition,
    parent,
    name,
    catalog_identity,
    validated
});
