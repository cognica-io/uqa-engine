//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Copying, Result, SyntaxCopy};

syntax_enum!(crate::ast::BinaryOp {
    Equal,
    NotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    Add,
    Subtract,
    Multiply,
    Divide
});

syntax_enum!(crate::ast::Expr {
    Star,
    QualifiedStar(value0),
    Default,
    Column(value0),
    QualifiedColumn { qualifier, column },
    InternalColumn(value0),
    Literal(value0),
    TypedLiteral { value, ty, collation_oid, composite_source },
    Param(value0),
    Func { name, binding, args, distinct, order_by, order_syntax, filter },
    Array(value0),
    Row(value0),
    CompositeRow { items, binding },
    Binary { op, lhs, rhs },
    UnaryMinus(value0),
    Not(value0),
    And(value0),
    Or(value0),
    IsNull { expr, negated },
    Between { expr, low, high },
    InList { expr, list, negated },
    WindowCall { name, args, spec, filter, modifiers },
    Case { base, when, else_branch },
    Cast { implicit, expr, ty },
    ScalarSubquery(value0),
    Exists { body, negated },
    InSubquery { expr, body, negated },
    RetainedLiteral(value0)
});

syntax_enum!(crate::ast::FrameBound {
    UnboundedPreceding,
    UnboundedFollowing,
    CurrentRow,
    Preceding(value0),
    Following(value0)
});

syntax_enum!(crate::ast::FrameExclusion {
    NoOthers,
    CurrentRow,
    Group,
    Ties
});

syntax_enum!(crate::ast::FrameMode {
    Rows,
    Range,
    Groups
});

syntax_enum!(crate::ast::NullsOrder { First, Last });

syntax_struct!(crate::ast::OrderBy {
    expr,
    descending,
    nulls
});

syntax_struct!(crate::ast::Projection { expr, alias });

syntax_struct!(crate::ast::WindowCallModifiers {
    distinct,
    ordered,
    within_group
});

syntax_struct!(crate::ast::WindowDefinition {
    name,
    inherited,
    spec
});

syntax_struct!(crate::ast::WindowFrame {
    mode,
    start,
    end,
    between,
    exclusion
});

syntax_struct!(crate::ast::WindowReference { name, kind });

syntax_enum!(crate::ast::WindowReferenceKind { Direct, Copy });

syntax_struct!(crate::ast::WindowSpec {
    raw_definition,
    definition,
    reference,
    partition_by,
    order_by,
    frame
});
