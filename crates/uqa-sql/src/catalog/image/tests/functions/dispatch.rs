//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{
    BooleanTest, EnumFunctionOperation, NumericOperator, PatternOperator, RangeFunctionOperation,
    RangeSubtype,
};
use std::num::NonZeroU32;

fn assert_dispatch(value: FunctionDispatch, payload: &[u8]) {
    let value = FunctionBinding {
        dispatch: Some(value),
        ..binding()
    };
    let bytes = encode(&value);
    assert_eq!(bytes[38], 1);
    assert_eq!(&bytes[39..bytes.len() - 1], payload);
    assert_eq!(bytes[bytes.len() - 1], 0);
    assert_restored(&value, &bytes);
}

#[test]
fn native_dispatch_images_preserve_every_fixed_operation_tag() {
    use FunctionDispatch as D;
    for (tag, value) in [
        (7, D::NamedArgument),
        (8, D::VariadicArgument),
        (9, D::ArraySubscripts),
        (10, D::ArraySlices),
        (12, D::Subscript),
        (13, D::Slice),
        (14, D::FieldSelect),
        (15, D::AssignmentStore),
        (16, D::AnyOperator),
        (17, D::AllOperator),
        (18, D::IsDistinct),
        (19, D::DistinctNot),
        (20, D::RowDistinct),
        (21, D::ValueIsDistinct),
        (23, D::BetweenSymmetric),
        (24, D::ToBinInt4),
        (25, D::ToBinInt8),
        (26, D::ToHexInt4),
        (27, D::ToHexInt8),
        (28, D::ToOctInt4),
        (29, D::ToOctInt8),
        (30, D::RandomInt4Range),
        (31, D::RandomInt8Range),
        (32, D::RandomNumericRange),
        (33, D::RegexSubstring),
        (34, D::SimilarSubstring),
        (35, D::ArraySortJson),
        (37, D::ArrayConcat),
        (38, D::ArrayAppend),
        (39, D::ArrayPrepend),
        (44, D::MergeAction),
    ] {
        assert_dispatch(value, &[tag]);
    }
    assert_dispatch(
        D::ArraySlicesWithBounds(ArraySliceBounds::from_stored_masks([0x81, 0xc2])),
        &[11, 0x81, 0xc2],
    );
    for flag in [false, true] {
        assert_dispatch(D::PatternEscape { similar: flag }, &[2, u8::from(flag)]);
        assert_dispatch(D::ValueNullTest { negated: flag }, &[22, u8::from(flag)]);
        for second in [false, true] {
            assert_dispatch(
                D::JsonPath {
                    as_text: flag,
                    jsonb: second,
                },
                &[40, u8::from(flag), u8::from(second)],
            );
            assert_dispatch(
                D::JsonExtract {
                    as_text: flag,
                    path: second,
                },
                &[41, u8::from(flag), u8::from(second)],
            );
        }
    }
}

#[test]
fn native_dispatch_images_keep_distinct_absent_zero_and_maximum_identifiers() {
    use FunctionDispatch as D;
    for oid in [None, Some(0), Some(950), Some(u32::MAX)] {
        let mut optional = vec![u8::from(oid.is_some())];
        if let Some(oid) = oid {
            optional.extend_from_slice(&oid.to_le_bytes());
        }
        for (tag, value) in [(4, D::Collate { oid }), (36, D::Concat { oid })] {
            let mut payload = vec![tag];
            payload.extend_from_slice(&optional);
            assert_dispatch(value, &payload);
        }
        for collation_oid in [0u32, 950, u32::MAX] {
            let mut payload = vec![5];
            payload.extend_from_slice(&collation_oid.to_le_bytes());
            payload.extend_from_slice(&optional);
            assert_dispatch(
                D::CollationFor {
                    collation_oid,
                    noncollatable_type_oid: oid,
                },
                &payload,
            );
        }
    }
    for type_oid in [0u32, 65536, u32::MAX] {
        let mut payload = vec![6];
        payload.extend_from_slice(&type_oid.to_le_bytes());
        assert_dispatch(D::TypeOf { type_oid }, &payload);
    }
}

#[test]
fn numeric_pattern_and_boolean_image_tags_match_the_native_operations() {
    use NumericOperator as N;
    for (tag, operator) in [
        N::Modulo,
        N::Power,
        N::Plus,
        N::SquareRoot,
        N::CubeRoot,
        N::Absolute,
        N::BitAnd,
        N::BitOr,
        N::BitXor,
        N::BitNot,
        N::ShiftLeft,
        N::ShiftRight,
    ]
    .into_iter()
    .enumerate()
    {
        assert_dispatch(FunctionDispatch::NumericOperator(operator), &[0, tag as u8]);
    }
    use PatternOperator as P;
    for (tag, operator) in [
        P::Like,
        P::NotLike,
        P::ILike,
        P::NotILike,
        P::Regex,
        P::NotRegex,
        P::RegexInsensitive,
        P::NotRegexInsensitive,
        P::StartsWith,
    ]
    .into_iter()
    .enumerate()
    {
        for oid in [None, NonZeroU32::new(1), NonZeroU32::new(u32::MAX)] {
            let mut payload = vec![1, tag as u8, u8::from(oid.is_some())];
            if let Some(oid) = oid {
                payload.extend_from_slice(&oid.get().to_le_bytes());
            }
            assert_dispatch(FunctionDispatch::Pattern { operator, oid }, &payload);
        }
    }
    use BooleanTest as B;
    for (tag, test) in [
        B::IsTrue,
        B::IsNotTrue,
        B::IsFalse,
        B::IsNotFalse,
        B::IsUnknown,
        B::IsNotUnknown,
    ]
    .into_iter()
    .enumerate()
    {
        assert_dispatch(FunctionDispatch::BooleanTest(test), &[3, tag as u8]);
    }
}

#[test]
fn range_and_enum_images_retain_selected_operation_subtype_and_identity() {
    use RangeFunctionOperation as R;
    use RangeSubtype as S;
    for (tag, operation) in [
        R::Lower,
        R::Upper,
        R::IsEmpty,
        R::LowerInclusive,
        R::UpperInclusive,
        R::LowerInfinite,
        R::UpperInfinite,
        R::Merge,
        R::Multirange,
        R::Overlap,
        R::Contains,
        R::ContainedBy,
        R::ContainsElement,
        R::ElementContainedBy,
        R::Adjacent,
    ]
    .into_iter()
    .enumerate()
    {
        for (kind, subtype) in [
            S::Integer,
            S::BigInteger,
            S::Numeric,
            S::Date,
            S::Timestamp,
            S::TimestampTz,
        ]
        .into_iter()
        .enumerate()
        {
            for multirange in [false, true] {
                assert_dispatch(
                    FunctionDispatch::Range {
                        operation,
                        subtype,
                        multirange,
                    },
                    &[42, tag as u8, kind as u8, u8::from(multirange)],
                );
            }
        }
    }
    use EnumFunctionOperation as E;
    for (tag, operation) in [
        E::First,
        E::Last,
        E::Range,
        E::BoundedRange,
        E::Compare,
        E::Equal,
        E::NotEqual,
        E::Less,
        E::Greater,
        E::LessEqual,
        E::GreaterEqual,
        E::Smaller,
        E::Larger,
        E::Hash,
        E::ExtendedHash,
    ]
    .into_iter()
    .enumerate()
    {
        for type_oid in [0u32, 65536, u32::MAX] {
            let mut payload = vec![43, tag as u8];
            payload.extend_from_slice(&type_oid.to_le_bytes());
            assert_dispatch(
                FunctionDispatch::Enum {
                    operation,
                    type_oid,
                },
                &payload,
            );
        }
    }
}
