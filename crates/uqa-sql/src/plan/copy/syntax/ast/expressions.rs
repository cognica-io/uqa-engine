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

macro_rules! syntax_expression {
    ($($variant:ident $(($($item:ident),+))? $({$($field:ident),*})?),* $(,)?) => {
        impl SyntaxCopy for crate::ast::Expr {
            fn copy_syntax(&self, copying: &mut Copying<'_>) -> Result<Self> {
                copying.check()?;
                if let Some(number) = copying.parameters.and_then(|parameters| parameters(self)) { return Ok(Self::Param(number)); }
                Ok(match self {
                    $(Self::$variant $(($($item),+))? $({$($field),*})? =>
                        Self::$variant $(($(copying.field(stringify!($item), |copying| $item.copy_syntax(copying))?),+))? $({$($field: copying.field(stringify!($field), |copying| $field.copy_syntax(copying))?),*})?),*
                })
            }
        }
    };
}

syntax_expression! {
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
}

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

impl SyntaxCopy for crate::ast::WindowSpec {
    fn copy_syntax(&self, copying: &mut Copying<'_>) -> Result<Self> {
        Ok(Self {
            raw_definition: copying.field("raw_definition", |copying| {
                if copying.parameters.is_some() {
                    Ok(None)
                } else {
                    self.raw_definition.copy_syntax(copying)
                }
            })?,
            definition: self.definition,
            reference: copying.field("reference", |copying| self.reference.copy_syntax(copying))?,
            partition_by: copying.field("partition_by", |copying| {
                self.partition_by.copy_syntax(copying)
            })?,
            order_by: copying.field("order_by", |copying| self.order_by.copy_syntax(copying))?,
            frame: copying.field("frame", |copying| self.frame.copy_syntax(copying))?,
        })
    }
}
