//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Copying, Result, SyntaxCopy};

syntax_struct!(crate::ast::AssignmentBinding {
    path,
    value_argument
});

syntax_enum!(crate::ast::AssignmentPathStep {
    Field { type_oid, number },
    Subscripts { first_argument, dimensions, slice, bounds }
});

syntax_enum!(crate::ast::AssignmentStep<crate::ast::Expr> {
    Field(value0),
    Index(value0),
    Slice { lower, upper }
});

syntax_struct!(crate::ast::AssignmentTarget {
    column,
    indirection
});

syntax_enum!(crate::ast::AssignmentTargets {
    Single(value0),
    Multiple(value0)
});

syntax_struct!(crate::ast::MultipleAssignmentTargets<crate::ast::Expr> { targets, source_positions, source_width });
