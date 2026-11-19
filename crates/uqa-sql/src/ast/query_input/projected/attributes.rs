//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    finish, frame, Fields, Input, JsonDiagnostic, JsonPosition, JsonReadError, Parts, Projected,
    Read, Result,
};
use crate::{
    ast::{ArraySliceBounds, MergeTargetColumnBinding, ReferenceName},
    catalog::collations::CollationSelection,
};
use std::fmt::Write;
use uqa_core::memory::ProductionString;

projected_struct!(MergeTargetColumnBinding {
    object_id: required,
    attribute_number: defaulted,
    domain_dependencies: defaulted
});

impl Projected for ReferenceName {
    fn read_value<'i, 'a>(
        input: Input<'i>,
        work: &'a mut crate::ast::query_input::projected::Work<'i>,
    ) -> Result<Read<'a, Self>> {
        frame(input.control, async move {
            let name = input.read::<String>(work).await?;
            let mut parts = Parts::new(input.control);
            let value = Self::from(parts.take(name));
            finish(parts, value)
        })
    }
}

impl Projected for ArraySliceBounds {
    fn read_value<'i, 'a>(
        input: Input<'i>,
        work: &'a mut crate::ast::query_input::projected::Work<'i>,
    ) -> Result<Read<'a, Self>> {
        frame(input.control, async move {
            let mut fields = Fields::new(
                input,
                &["omitted_lower", "omitted_upper"],
                [3, 3],
                "struct ArraySliceBounds",
                false,
                work,
            )
            .await?;
            let mut masks = [0; 2];
            while let Some((index, value)) = fields.next(work).await? {
                if let Some(index) = index {
                    masks[index] = input.child(value).read::<u8>(work).await?.into_parts().0;
                }
            }
            input.scalar(Self::from_stored_masks(masks))
        })
    }
}

impl Projected for CollationSelection {
    fn read_value<'i, 'a>(
        input: Input<'i>,
        work: &'a mut crate::ast::query_input::projected::Work<'i>,
    ) -> Result<Read<'a, Self>> {
        frame(input.control, async move {
            let oid = input.read::<u32>(work).await?.into_parts().0;
            if let Some(value) = Self::from_oid(oid) {
                return input.scalar(value);
            }
            let mut text = ProductionString::new(input.control);
            let _ = write!(text, "{oid}");
            Err(JsonDiagnostic::retained_message(
                &const {
                    uqa_core::json::JsonMessageFormat::new(
                        "collation ",
                        " is not a built-in collation",
                        "collation is not a built-in collation",
                    )
                },
                text.finish().map_err(JsonReadError::from)?,
                JsonPosition::unlocated(),
            )
            .into())
        })
    }
}

use crate::ast::{EnumFunctionOperation, PatternOperator};

projected_enum!(PatternOperator {
    Like,
    NotLike,
    ILike,
    NotILike,
    Regex,
    NotRegex,
    RegexInsensitive,
    NotRegexInsensitive,
    StartsWith
});

projected_enum!(EnumFunctionOperation {
    First,
    Last,
    Range,
    BoundedRange,
    Compare,
    Equal,
    NotEqual,
    Less,
    Greater,
    LessEqual,
    GreaterEqual,
    Smaller,
    Larger,
    Hash,
    ExtendedHash
});
