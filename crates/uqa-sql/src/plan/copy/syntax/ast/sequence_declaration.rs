//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Copying, Result, SyntaxCopy};

syntax_struct!(crate::ast::SequenceDeclaration {
    data_type,
    increment,
    cycle,
    max_value,
    min_value,
    start,
    restart,
    cache,
    owned_by
});

syntax_enum!(crate::ast::SequenceOptionValue {
    Absent,
    Integer(value0),
    Text(value0)
});
