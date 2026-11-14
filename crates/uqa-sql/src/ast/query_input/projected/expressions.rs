//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::ast::{BinaryOp, Expr, FunctionCallSyntax, InternalColumnRef};
use crate::expr::composites::{
    CompositeAttribute, CompositeConstantSource, CompositeTypeDescriptor, DroppedCompositeAttribute,
};

projected_enum!(Expr {
    Star,
    QualifiedStar(value),
    Default,
    Column(value),
    QualifiedColumn { qualifier: required, column: required },
    InternalColumn(value),
    Literal(value),
    TypedLiteral { value: required, ty: required, collation_oid: defaulted, composite_source: defaulted },
    Param(value),
    Func { name: required, binding: defaulted, args: required, distinct: required, order_by: required, order_syntax: defaulted, filter: optional },
    Array(value),
    Row(value),
    CompositeRow { items: required, binding: required },
    Binary { op: required, lhs: required, rhs: required },
    UnaryMinus(value),
    Not(value),
    And(value),
    Or(value),
    IsNull { expr: required, negated: required },
    Between { expr: required, low: required, high: required },
    InList { expr: required, list: required, negated: required },
    WindowCall { name: required, args: required, spec: required, filter: defaulted, modifiers: defaulted },
    Case { base: optional, when: required, else_branch: optional },
    Cast { implicit: defaulted, expr: required, ty: required },
    ScalarSubquery(value),
    Exists { body: required, negated: required },
    InSubquery { expr: required, body: required, negated: required }
} skip { RetainedLiteral(value) } untagged("Expr"));

impl super::Projected for InternalColumnRef {
    fn read_value<'i, 'a>(
        input: super::Input<'i>,
        work: &'a mut crate::ast::query_input::projected::Work<'i>,
    ) -> super::Result<super::Read<'a, Self>> {
        super::frame(input.control, async move {
            let mut fields = super::Fields::new(
                input,
                &["relation", "attribute"],
                [3, 3],
                "struct InternalColumnRef",
                false,
                work,
            )
            .await?;
            let (mut relation, mut attribute) = (None, None);
            while let Some((index, value)) = fields.next(work).await? {
                let child = input.child(value);
                match index {
                    Some(0) => relation = Some(child.read::<u64>(work).await?.into_parts().0),
                    Some(1) => attribute = Some(child.read::<u32>(work).await?.into_parts().0),
                    None => (),
                    _ => unreachable!("internal column fields"),
                }
            }
            input.scalar(Self::from_raw(
                relation.expect("completed relation"),
                attribute.expect("completed attribute"),
            ))
        })
    }
}

projected_enum!(FunctionCallSyntax {
    Legacy,
    Ordinary,
    WithinGroup,
    Extract
});

projected_enum!(BinaryOp {
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

projected_struct!(CompositeConstantSource {
    value: required,
    descriptors: required
});

projected_struct!(CompositeTypeDescriptor {
    dropped: defaulted,
    type_oid: required,
    relation_oid: required,
    attributes: required
});

projected_struct!(CompositeAttribute {
    name: required,
    ty: required,
    collation_oid: defaulted,
    number: required
});

projected_struct!(DroppedCompositeAttribute {
    number: required,
    length: required,
    alignment: required
});
